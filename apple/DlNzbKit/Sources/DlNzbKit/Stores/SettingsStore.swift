import Foundation
import Observation

/// Every setting in the Settings window (Mac) and sheet (iPhone, iPad), kept
/// in `UserDefaults` as it changes, with the server password in the Keychain.
///
/// The Keychain is read and written off the main actor, one operation at a
/// time in the order they were asked for: the password is read as the store
/// is made (`passwordLoaded()` waits for it), and written shortly after the
/// typing stops. `keychainSettled()` waits for everything asked so far.
///
/// Launch arguments land in the defaults' argument domain, so any setting can
/// be overridden for a run: `-serverHost news.example.com -startAutomatically NO`.
@MainActor
@Observable
public final class SettingsStore {
  public enum Key {
    public static let downloadFolderBookmark = "downloadFolderBookmark"
    public static let startAutomatically = "startAutomatically"
    public static let retention = "retention"
    public static let notifyWhenFinished = "notifyWhenFinished"
    public static let preventSleep = "preventSleep"
    public static let showInMenuBar = "showInMenuBar"
    public static let host = "serverHost"
    public static let port = "serverPort"
    public static let portEdited = "serverPortEdited"
    public static let useSSL = "serverUseSSL"
    public static let username = "serverUsername"
    public static let connections = "serverConnections"
    public static let passwordAccount = "serverPasswordAccount"
    public static let repairWithPar2 = "repairWithPar2"
    public static let extractArchives = "extractArchives"
    public static let deleteArchivesAfterExtracting = "deleteArchivesAfterExtracting"
    public static let deletePar2AfterRepairing = "deletePar2AfterRepairing"
    public static let renameObfuscatedFiles = "renameObfuscatedFiles"
    public static let preflight = "preflight"
    public static let downloadAllRecoveryUpFront = "downloadAllRecoveryUpFront"
    public static let limitsSpeed = "limitsSpeed"
    public static let speedLimitMegabytesPerSecond = "speedLimitMegabytesPerSecond"
    public static let verifyCertificate = "verifyCertificate"
    public static let retryAttempts = "retryAttempts"
    public static let flushFilesWhenFinished = "flushFilesWhenFinished"

    static let all = [
      downloadFolderBookmark, startAutomatically, retention, notifyWhenFinished, preventSleep, showInMenuBar, host, port, portEdited, useSSL,
      username, connections, passwordAccount, repairWithPar2, extractArchives, deleteArchivesAfterExtracting, deletePar2AfterRepairing,
      renameObfuscatedFiles, preflight, downloadAllRecoveryUpFront, limitsSpeed, speedLimitMegabytesPerSecond, verifyCertificate,
      retryAttempts, flushFilesWhenFinished,
    ]
  }

  public static let defaultSpeedLimitMegabytesPerSecond: Double = 20

  /// The shipped values of the settings no settings value
  /// (`ServerSettings`, `ProcessingSettings`, `AdvancedSettings`) has a
  /// default for: what a fresh install reads and Reset puts back.
  private enum Defaults {
    static let startAutomatically = true
    static let retention = RetentionPolicy.manually
    static let notifyWhenFinished = true
    static let preventSleep = true
    static let showInMenuBar = false
    static let limitsSpeed = false
  }

  @ObservationIgnored private let defaults: UserDefaults
  @ObservationIgnored private let passwords: any PasswordStore
  /// The account the Keychain item is stored under now, so a changed host or
  /// username moves the password instead of leaving the old item behind.
  @ObservationIgnored private var savedAccount: NNTPAccount?
  @ObservationIgnored private var passwordSaveTask: Task<Void, Never>?
  /// The latest Keychain operation; the next one waits for it.
  @ObservationIgnored private var keychainWork: Task<Void, Never>?
  /// Reading the stored password, from the store's making until it is in
  /// `password`.
  @ObservationIgnored private var passwordLoad: Task<Void, Never>?
  @ObservationIgnored private var isLoadingPassword = false
  /// Set while the stored password is put in place, so that is not an edit.
  @ObservationIgnored private var applyingLoadedPassword = false
  /// The password has been typed, imported or reset since the store was
  /// made, so the stored one, if it arrives later, must not replace it.
  @ObservationIgnored private var passwordEdited = false
  /// Set while the port follows SSL, so that change does not count as an edit.
  @ObservationIgnored private var adjustingPort = false
  /// Set while resetting, so the didSets do not schedule Keychain work.
  @ObservationIgnored private var resetting = false
  @ObservationIgnored private var accessedFolder: URL?

  // MARK: General

  /// Each download gets its own folder in here. On the Mac, a security-scoped
  /// bookmark the store keeps open; on iPhone and iPad, Documents/Downloads.
  public private(set) var downloadFolder: URL

  public var startAutomatically: Bool {
    didSet { defaults.set(startAutomatically, forKey: Key.startAutomatically) }
  }

  public var retention: RetentionPolicy {
    didSet { defaults.set(retention.rawValue, forKey: Key.retention) }
  }

  public var notifyWhenFinished: Bool {
    didSet { defaults.set(notifyWhenFinished, forKey: Key.notifyWhenFinished) }
  }

  /// Mac: hold off idle sleep and App Nap while anything downloads.
  public var preventSleep: Bool {
    didSet { defaults.set(preventSleep, forKey: Key.preventSleep) }
  }

  /// Mac only.
  public var showInMenuBar: Bool {
    didSet { defaults.set(showInMenuBar, forKey: Key.showInMenuBar) }
  }

  // MARK: Server

  public var host: String {
    didSet {
      defaults.set(host, forKey: Key.host)
      schedulePasswordSave()
    }
  }

  /// Follows SSL (563 or 119) until the user types a port of their own.
  public var port: Int {
    didSet {
      defaults.set(port, forKey: Key.port)
      if !adjustingPort {
        portEdited = port != ServerSettings.defaultPort(useSSL: useSSL)
      }
      schedulePasswordSave()
    }
  }

  /// Whether the port is the user's own rather than the one SSL implies.
  public private(set) var portEdited: Bool {
    didSet { defaults.set(portEdited, forKey: Key.portEdited) }
  }

  public var useSSL: Bool {
    didSet {
      defaults.set(useSSL, forKey: Key.useSSL)
      if !portEdited {
        adjustingPort = true
        port = ServerSettings.defaultPort(useSSL: useSSL)
        adjustingPort = false
      }
    }
  }

  public var username: String {
    didSet {
      defaults.set(username, forKey: Key.username)
      schedulePasswordSave()
    }
  }

  /// In the Keychain, written shortly after the last change (or at once with
  /// `savePasswordNow()`), never in the defaults. Empty until the Keychain
  /// has been read (`passwordLoaded()`).
  public var password: String {
    didSet {
      guard !applyingLoadedPassword else { return }
      passwordEdited = true
      schedulePasswordSave()
    }
  }

  public var connections: Int {
    didSet { defaults.set(connections, forKey: Key.connections) }
  }

  // MARK: Processing

  public var repairWithPar2: Bool {
    didSet { defaults.set(repairWithPar2, forKey: Key.repairWithPar2) }
  }

  public var extractArchives: Bool {
    didSet { defaults.set(extractArchives, forKey: Key.extractArchives) }
  }

  public var deleteArchivesAfterExtracting: Bool {
    didSet { defaults.set(deleteArchivesAfterExtracting, forKey: Key.deleteArchivesAfterExtracting) }
  }

  public var deletePar2AfterRepairing: Bool {
    didSet { defaults.set(deletePar2AfterRepairing, forKey: Key.deletePar2AfterRepairing) }
  }

  public var renameObfuscatedFiles: Bool {
    didSet { defaults.set(renameObfuscatedFiles, forKey: Key.renameObfuscatedFiles) }
  }

  // MARK: Advanced

  public var preflight: Preflight {
    didSet { defaults.set(preflight.rawValue, forKey: Key.preflight) }
  }

  public var downloadAllRecoveryUpFront: Bool {
    didSet { defaults.set(downloadAllRecoveryUpFront, forKey: Key.downloadAllRecoveryUpFront) }
  }

  public var limitsSpeed: Bool {
    didSet { defaults.set(limitsSpeed, forKey: Key.limitsSpeed) }
  }

  /// The limit while `limitsSpeed` is on, in decimal megabytes a second (as
  /// the status line shows speeds).
  public var speedLimitMegabytesPerSecond: Double {
    didSet { defaults.set(speedLimitMegabytesPerSecond, forKey: Key.speedLimitMegabytesPerSecond) }
  }

  public var verifyCertificate: Bool {
    didSet { defaults.set(verifyCertificate, forKey: Key.verifyCertificate) }
  }

  public var retryAttempts: Int {
    didSet { defaults.set(retryAttempts, forKey: Key.retryAttempts) }
  }

  public var flushFilesWhenFinished: Bool {
    didSet { defaults.set(flushFilesWhenFinished, forKey: Key.flushFilesWhenFinished) }
  }

  // MARK: Life

  public init(defaults: UserDefaults = .standard, passwords: any PasswordStore = Keychain()) {
    self.defaults = defaults
    self.passwords = passwords

    func bool(_ key: String, _ fallback: Bool) -> Bool {
      // bool(forKey:) also reads "YES" and "NO" from launch arguments, which
      // an `as? Bool` cast ignores.
      defaults.object(forKey: key) == nil ? fallback : defaults.bool(forKey: key)
    }
    func int(_ key: String, _ fallback: Int, in range: ClosedRange<Int>) -> Int {
      defaults.object(forKey: key) == nil ? fallback : defaults.integer(forKey: key).clamped(to: range)
    }

    let server = ServerSettings()
    let processing = ProcessingSettings()
    let advanced = AdvancedSettings()

    startAutomatically = bool(Key.startAutomatically, Defaults.startAutomatically)
    retention = RetentionPolicy(rawValue: defaults.string(forKey: Key.retention) ?? "") ?? Defaults.retention
    notifyWhenFinished = bool(Key.notifyWhenFinished, Defaults.notifyWhenFinished)
    preventSleep = bool(Key.preventSleep, Defaults.preventSleep)
    showInMenuBar = bool(Key.showInMenuBar, Defaults.showInMenuBar)

    let useSSL = bool(Key.useSSL, server.useSSL)
    let host = defaults.string(forKey: Key.host) ?? ""
    let port = int(Key.port, ServerSettings.defaultPort(useSSL: useSSL), in: 1...65_535)
    let username = defaults.string(forKey: Key.username) ?? ""
    self.useSSL = useSSL
    self.host = host
    self.port = port
    portEdited = bool(Key.portEdited, false)
    self.username = username
    connections = int(Key.connections, server.connections, in: ServerSettings.connectionRange)
    password = ""
    savedAccount = Self.storedAccount(in: defaults)

    repairWithPar2 = bool(Key.repairWithPar2, processing.repairWithPar2)
    extractArchives = bool(Key.extractArchives, processing.extractArchives)
    deleteArchivesAfterExtracting = bool(Key.deleteArchivesAfterExtracting, processing.deleteArchivesAfterExtracting)
    deletePar2AfterRepairing = bool(Key.deletePar2AfterRepairing, processing.deletePar2AfterRepairing)
    renameObfuscatedFiles = bool(Key.renameObfuscatedFiles, processing.renameObfuscatedFiles)

    preflight = Preflight(rawValue: defaults.string(forKey: Key.preflight) ?? "") ?? advanced.preflight
    downloadAllRecoveryUpFront = bool(Key.downloadAllRecoveryUpFront, advanced.downloadAllRecoveryUpFront)
    limitsSpeed = bool(Key.limitsSpeed, Defaults.limitsSpeed)
    let storedLimit = defaults.double(forKey: Key.speedLimitMegabytesPerSecond)
    speedLimitMegabytesPerSecond = storedLimit > 0 ? storedLimit : Self.defaultSpeedLimitMegabytesPerSecond
    verifyCertificate = bool(Key.verifyCertificate, server.verifyCertificate)
    retryAttempts = int(Key.retryAttempts, server.retryAttempts, in: ServerSettings.retryRange)
    flushFilesWhenFinished = bool(Key.flushFilesWhenFinished, advanced.flushFilesWhenFinished)

    let folder = Self.resolveDownloadFolder(defaults: defaults)
    downloadFolder = folder.url
    if folder.scoped { beginAccess(to: folder.url) }
    if hasServer { loadPassword() }
  }

  // MARK: Derived

  /// A server is configured: the queue holds off until there is one.
  public var hasServer: Bool {
    !host.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
  }

  public var serverSettings: ServerSettings {
    ServerSettings(
      host: ServerSettings(host: host).normalisedHost, port: port, useSSL: useSSL, verifyCertificate: verifyCertificate, username: username,
      connections: connections, retryAttempts: retryAttempts)
  }

  public var processingSettings: ProcessingSettings {
    ProcessingSettings(
      repairWithPar2: repairWithPar2, extractArchives: extractArchives, deleteArchivesAfterExtracting: deleteArchivesAfterExtracting,
      deletePar2AfterRepairing: deletePar2AfterRepairing, renameObfuscatedFiles: renameObfuscatedFiles)
  }

  public var advancedSettings: AdvancedSettings {
    AdvancedSettings(preflight: preflight, downloadAllRecoveryUpFront: downloadAllRecoveryUpFront, flushFilesWhenFinished: flushFilesWhenFinished)
  }

  /// Nil when unlimited.
  public var speedLimitBytesPerSecond: Int64? {
    guard limitsSpeed, speedLimitMegabytesPerSecond > 0 else { return nil }
    return Int64((speedLimitMegabytesPerSecond * 1_000_000).rounded())
  }

  /// Everything the engine is configured with. Reading it in an observation
  /// tracks every setting the engine cares about.
  public var engineSettings: EngineSettings {
    var server = serverSettings
    #if DEBUG
      // `-connectAddress host:port` (testing only) connects through a relay,
      // such as one a test cuts to take this app offline, while the password
      // stays filed under the real server. The relay's address can't match
      // the server's certificate, so certificate checks are off with it.
      if let address = UserDefaults.standard.string(forKey: "connectAddress"),
        let colon = address.lastIndex(of: ":"), let relayPort = Int(address[address.index(after: colon)...])
      {
        server.host = String(address[..<colon])
        server.port = relayPort
        server.verifyCertificate = false
      }
    #endif
    return EngineSettings(
      server: server, password: password, processing: processingSettings, advanced: advancedSettings,
      speedLimitBytesPerSecond: speedLimitBytesPerSecond)
  }

  // MARK: Actions

  /// Takes on the CLI's settings. The CLI's download folder is left alone: the
  /// sandbox needs the user to choose a folder before the app may write there.
  public func apply(_ imported: ImportedSettings) {
    let server = imported.server
    useSSL = server.useSSL
    host = server.host
    port = server.port
    username = server.username
    password = imported.password
    connections = server.connections.clamped(to: ServerSettings.connectionRange)
    verifyCertificate = server.verifyCertificate
    retryAttempts = server.retryAttempts.clamped(to: ServerSettings.retryRange)
    repairWithPar2 = imported.processing.repairWithPar2
    extractArchives = imported.processing.extractArchives
    deleteArchivesAfterExtracting = imported.processing.deleteArchivesAfterExtracting
    deletePar2AfterRepairing = imported.processing.deletePar2AfterRepairing
    renameObfuscatedFiles = imported.processing.renameObfuscatedFiles
    downloadAllRecoveryUpFront = imported.advanced.downloadAllRecoveryUpFront
    flushFilesWhenFinished = imported.advanced.flushFilesWhenFinished
    savePasswordNow()
    Log.settings.info("imported the dl-nzb CLI settings for \(server.host, privacy: .public)")
  }

  /// Every setting back to its default, the password out of the Keychain, and
  /// the download folder back to the default one.
  public func resetAll() {
    passwordSaveTask?.cancel()
    passwordSaveTask = nil
    let accounts = [savedAccount, currentAccount].compactMap { $0 }
    onKeychain { passwords in
      for account in accounts { passwords.deletePassword(for: account) }
    }
    savedAccount = nil
    resetting = true
    defer { resetting = false }

    let server = ServerSettings()
    let processing = ProcessingSettings()
    let advanced = AdvancedSettings()
    startAutomatically = Defaults.startAutomatically
    retention = Defaults.retention
    notifyWhenFinished = Defaults.notifyWhenFinished
    preventSleep = Defaults.preventSleep
    showInMenuBar = Defaults.showInMenuBar
    useSSL = server.useSSL
    host = ""
    port = server.port
    username = ""
    password = ""
    connections = server.connections
    repairWithPar2 = processing.repairWithPar2
    extractArchives = processing.extractArchives
    deleteArchivesAfterExtracting = processing.deleteArchivesAfterExtracting
    deletePar2AfterRepairing = processing.deletePar2AfterRepairing
    renameObfuscatedFiles = processing.renameObfuscatedFiles
    preflight = advanced.preflight
    downloadAllRecoveryUpFront = advanced.downloadAllRecoveryUpFront
    limitsSpeed = Defaults.limitsSpeed
    speedLimitMegabytesPerSecond = Self.defaultSpeedLimitMegabytesPerSecond
    verifyCertificate = server.verifyCertificate
    retryAttempts = server.retryAttempts
    flushFilesWhenFinished = advanced.flushFilesWhenFinished
    for key in Key.all {
      defaults.removeObject(forKey: key)
    }
    portEdited = false
    defaults.removeObject(forKey: Key.portEdited)
    useDefaultDownloadFolder()
    Log.settings.info("reset every setting")
  }

  // MARK: Password

  private var currentAccount: NNTPAccount {
    NNTPAccount(server: ServerSettings(host: host).normalisedHost, port: port, username: username)
  }

  /// Typing a host or a password changes it a character at a time; the
  /// Keychain is written once the typing stops.
  private func schedulePasswordSave() {
    guard !resetting else { return }
    passwordSaveTask?.cancel()
    passwordSaveTask = Task { [weak self] in
      try? await Task.sleep(for: .milliseconds(600))
      guard !Task.isCancelled else { return }
      self?.savePasswordNow()
    }
  }

  /// Writes the password under the current server and username, and removes
  /// the item stored under the previous ones. The Keychain work happens in
  /// the background, in order; `keychainSettled()` waits for it.
  public func savePasswordNow() {
    passwordSaveTask?.cancel()
    passwordSaveTask = nil
    if isLoadingPassword && !passwordEdited {
      // The stored password is not here yet; saving now would save none.
      passwordSaveTask = Task { [weak self] in
        await self?.passwordLoaded()
        self?.savePasswordNow()
      }
      return
    }
    let account = currentAccount
    let password = password
    if let previous = savedAccount, previous != account {
      onKeychain { $0.deletePassword(for: previous) }
      savedAccount = nil
    }
    guard !account.server.isEmpty else {
      storeAccount(nil)
      return
    }
    if password.isEmpty {
      onKeychain { $0.deletePassword(for: account) }
      storeAccount(nil)
      return
    }
    storeAccount(account)
    onKeychain { passwords in
      do {
        try passwords.setPassword(password, for: account)
      } catch {
        Log.settings.error("the server password was not saved: \(error.localizedDescription, privacy: .public)")
      }
    }
  }

  /// Waits until the password stored in the Keychain has been read into
  /// `password` (or found missing). The engine's settings and Test
  /// Connection wait for this, so neither goes out without the password.
  public func passwordLoaded() async {
    await passwordLoad?.value
  }

  /// Waits until every Keychain read and write asked for so far is done.
  public func keychainSettled() async {
    while let work = keychainWork {
      await work.value
      if keychainWork == work { return }
    }
  }

  /// Reads the stored password in the background, keeping whatever the user
  /// types meanwhile.
  private func loadPassword() {
    let account = currentAccount
    let read = onKeychain { $0.password(for: account) }
    isLoadingPassword = true
    passwordLoad = Task { [weak self] in
      let stored = await read.value
      guard let self else { return }
      self.isLoadingPassword = false
      guard !self.passwordEdited else { return }
      self.applyingLoadedPassword = true
      self.password = stored ?? ""
      self.applyingLoadedPassword = false
    }
  }

  /// Runs Keychain work off the main actor, after the work asked for before it.
  @discardableResult
  private func onKeychain<Result: Sendable>(_ work: @escaping @Sendable (any PasswordStore) -> Result) -> Task<Result, Never> {
    let previous = keychainWork
    let passwords = passwords
    let task = Task.detached(priority: .userInitiated) {
      await previous?.value
      return work(passwords)
    }
    keychainWork = Task { _ = await task.value }
    return task
  }

  private func storeAccount(_ account: NNTPAccount?) {
    savedAccount = account
    if let account, let data = try? JSONEncoder().encode(account) {
      defaults.set(data, forKey: Key.passwordAccount)
    } else {
      defaults.removeObject(forKey: Key.passwordAccount)
    }
  }

  private static func storedAccount(in defaults: UserDefaults) -> NNTPAccount? {
    defaults.data(forKey: Key.passwordAccount).flatMap { try? JSONDecoder().decode(NNTPAccount.self, from: $0) }
  }

  // MARK: Download folder

  #if os(macOS)
    /// A folder the user chose in an open panel. Its bookmark keeps access
    /// across launches; the store holds the folder open from now on.
    public func setDownloadFolder(_ url: URL) throws {
      let data = try Self.bookmark(for: url)
      defaults.set(data, forKey: Key.downloadFolderBookmark)
      beginAccess(to: url)
      downloadFolder = url
      Log.settings.info("download folder is now \(url.path(percentEncoded: false), privacy: .public)")
    }

    private static func bookmark(for url: URL) throws -> Data {
      do {
        return try url.bookmarkData(options: [.withSecurityScope], includingResourceValuesForKeys: nil, relativeTo: nil)
      } catch {
        // Outside the sandbox (tests, command-line runs) a plain bookmark does.
        return try url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil)
      }
    }
  #endif

  /// Back to ~/Downloads (Mac) or Documents/Downloads (iPhone, iPad).
  public func useDefaultDownloadFolder() {
    defaults.removeObject(forKey: Key.downloadFolderBookmark)
    endAccess()
    downloadFolder = AppPaths.defaultDownloadFolder
  }

  /// The bookmarked folder (refreshing a stale bookmark), or the default one.
  private static func resolveDownloadFolder(defaults: UserDefaults) -> (url: URL, scoped: Bool) {
    #if os(macOS)
      guard let data = defaults.data(forKey: Key.downloadFolderBookmark) else {
        return (AppPaths.defaultDownloadFolder, false)
      }
      var stale = false
      let url: URL
      do {
        url = try URL(resolvingBookmarkData: data, options: [.withSecurityScope], relativeTo: nil, bookmarkDataIsStale: &stale)
      } catch {
        do {
          url = try URL(resolvingBookmarkData: data, options: [], relativeTo: nil, bookmarkDataIsStale: &stale)
        } catch {
          Log.settings.error("the download folder could not be found, using Downloads: \(error.localizedDescription, privacy: .public)")
          defaults.removeObject(forKey: Key.downloadFolderBookmark)
          return (AppPaths.defaultDownloadFolder, false)
        }
      }
      if stale {
        // A moved or renamed folder resolves with a stale bookmark; a fresh
        // one needs access, which a security-scoped resolve grants.
        if let fresh = url.withSecurityScopedAccess({ try? bookmark(for: url) }) {
          defaults.set(fresh, forKey: Key.downloadFolderBookmark)
        }
      }
      return (url, true)
    #else
      return (AppPaths.defaultDownloadFolder, false)
    #endif
  }

  private func beginAccess(to url: URL) {
    endAccess()
    if url.startAccessingSecurityScopedResource() {
      accessedFolder = url
    }
  }

  private func endAccess() {
    accessedFolder?.stopAccessingSecurityScopedResource()
    accessedFolder = nil
  }

  // MARK: Previews

  public nonisolated static let previewSuiteName = "com.zephleggett.dl-nzb.preview"

  /// A store with the shipped defaults (and optionally a server), for
  /// previews and tests. It never touches the real preferences or the Keychain.
  public static func preview(
    host: String = "news.example.com", username: String = "zeph", password: String = "secret", downloadFolder: URL? = nil
  ) -> SettingsStore {
    let defaults = UserDefaults(suiteName: previewSuiteName) ?? .standard
    defaults.removePersistentDomain(forName: previewSuiteName)
    let store = SettingsStore(defaults: defaults, passwords: InMemoryPasswordStore())
    store.host = host
    store.username = username
    store.password = password
    store.passwordSaveTask?.cancel()
    if let downloadFolder { store.downloadFolder = downloadFolder }
    return store
  }
}
