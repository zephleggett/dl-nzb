import Foundation
import Synchronization
import Testing

@testable import DlNzbKit

/// Settings: what they start as, that they persist, the port following SSL,
/// and the password living in the password store under the right account.
@MainActor
@Suite("Settings store")
struct SettingsStoreTests {
  /// A defaults suite of the test's own, removed afterwards.
  private func withDefaults(_ body: (UserDefaults) throws -> Void) throws {
    let suite = "com.zephleggett.dl-nzb.tests.\(UUID().uuidString)"
    let defaults = try #require(UserDefaults(suiteName: suite))
    defer { defaults.removePersistentDomain(forName: suite) }
    try body(defaults)
  }

  /// The same, for tests that wait on the Keychain.
  private func withDefaults(_ body: @MainActor (UserDefaults) async throws -> Void) async throws {
    let suite = "com.zephleggett.dl-nzb.tests.\(UUID().uuidString)"
    let defaults = try #require(UserDefaults(suiteName: suite))
    defer { defaults.removePersistentDomain(forName: suite) }
    try await body(defaults)
  }

  @Test("A fresh store has the SPEC's defaults")
  func defaults() throws {
    try withDefaults { defaults in
      let settings = SettingsStore(defaults: defaults, passwords: InMemoryPasswordStore())
      #expect(settings.startAutomatically)
      #expect(settings.retention == .manually)
      #expect(settings.notifyWhenFinished && settings.preventSleep && !settings.showInMenuBar)
      #expect(settings.host.isEmpty && !settings.hasServer)
      #expect(settings.port == 563 && settings.useSSL && !settings.portEdited)
      #expect(settings.connections == 30)
      #expect(settings.processingSettings == ProcessingSettings())
      #expect(settings.preflight == .automatic && !settings.downloadAllRecoveryUpFront)
      #expect(!settings.limitsSpeed && settings.speedLimitBytesPerSecond == nil)
      #expect(settings.verifyCertificate && settings.retryAttempts == 2 && !settings.flushFilesWhenFinished)
      #expect(settings.downloadFolder == AppPaths.defaultDownloadFolder)
    }
  }

  @Test("Every setting is still there in a new store")
  func persistence() throws {
    try withDefaults { defaults in
      let passwords = InMemoryPasswordStore()
      let settings = SettingsStore(defaults: defaults, passwords: passwords)
      settings.startAutomatically = false
      settings.retention = .afterOneDay
      settings.notifyWhenFinished = false
      settings.showInMenuBar = true
      settings.host = "news.example.com"
      settings.username = "zeph"
      settings.connections = 45
      settings.extractArchives = false
      settings.preflight = .always
      settings.limitsSpeed = true
      settings.speedLimitMegabytesPerSecond = 12.5
      settings.retryAttempts = 5
      settings.flushFilesWhenFinished = true

      let reread = SettingsStore(defaults: defaults, passwords: passwords)
      #expect(!reread.startAutomatically)
      #expect(reread.retention == .afterOneDay)
      #expect(!reread.notifyWhenFinished && reread.showInMenuBar)
      #expect(reread.host == "news.example.com" && reread.username == "zeph" && reread.hasServer)
      #expect(reread.connections == 45 && !reread.extractArchives && reread.preflight == .always)
      #expect(reread.speedLimitBytesPerSecond == 12_500_000)
      #expect(reread.retryAttempts == 5 && reread.flushFilesWhenFinished)
    }
  }

  @Test("The port follows SSL until the user types one of their own")
  func portFollowsSSL() throws {
    try withDefaults { defaults in
      let settings = SettingsStore(defaults: defaults, passwords: InMemoryPasswordStore())
      settings.useSSL = false
      #expect(settings.port == 119 && !settings.portEdited)
      settings.useSSL = true
      #expect(settings.port == 563)
      settings.port = 443
      #expect(settings.portEdited)
      settings.useSSL = false
      #expect(settings.port == 443)
      #expect(SettingsStore(defaults: defaults, passwords: InMemoryPasswordStore()).portEdited)
      // Typing the port SSL would choose makes it follow again.
      settings.port = 119
      #expect(!settings.portEdited)
      settings.useSSL = true
      #expect(settings.port == 563)
    }
  }

  @Test("The password is stored under the server, port and username, and moves when they change")
  func passwordMoves() async throws {
    try await withDefaults { defaults in
      let passwords = InMemoryPasswordStore()
      let settings = SettingsStore(defaults: defaults, passwords: passwords)
      settings.host = "news.example.com"
      settings.username = "zeph"
      settings.password = "secret"
      settings.savePasswordNow()
      await settings.keychainSettled()
      let first = NNTPAccount(server: "news.example.com", port: 563, username: "zeph")
      #expect(passwords.accounts == [first])
      #expect(passwords.password(for: first) == "secret")
      #expect(defaults.string(forKey: "password") == nil)
      let reread = SettingsStore(defaults: defaults, passwords: passwords)
      await reread.passwordLoaded()
      #expect(reread.password == "secret")

      settings.host = "eu.example.com"
      settings.savePasswordNow()
      await settings.keychainSettled()
      let moved = NNTPAccount(server: "eu.example.com", port: 563, username: "zeph")
      #expect(passwords.accounts == [moved])

      settings.password = ""
      settings.savePasswordNow()
      await settings.keychainSettled()
      #expect(passwords.accounts.isEmpty)
    }
  }

  @Test("The stored password is read off the main actor, and never replaces one typed meanwhile")
  func passwordLoadsInBackground() async throws {
    try await withDefaults { defaults in
      let passwords = SlowPasswordStore()
      let account = NNTPAccount(server: "news.example.com", port: 563, username: "zeph")
      try passwords.setPassword("stored", for: account)
      defaults.set("news.example.com", forKey: SettingsStore.Key.host)
      defaults.set("zeph", forKey: SettingsStore.Key.username)

      let settings = SettingsStore(defaults: defaults, passwords: passwords)
      // Made at once, with the read still under way.
      #expect(settings.password.isEmpty)
      await settings.passwordLoaded()
      #expect(settings.password == "stored")
      #expect(passwords.readOnMainThread == false)

      let typing = SettingsStore(defaults: defaults, passwords: passwords)
      typing.password = "typed"
      await typing.passwordLoaded()
      #expect(typing.password == "typed")

      // Saving before the read is done waits for it, rather than saving none.
      let early = SettingsStore(defaults: defaults, passwords: passwords)
      early.savePasswordNow()
      await early.passwordLoaded()
      #expect(await eventually { passwords.password(for: account) == "stored" })
    }
  }

  @Test("The password is written once the typing stops")
  func passwordDebounce() async throws {
    let passwords = InMemoryPasswordStore()
    let suite = "com.zephleggett.dl-nzb.tests.\(UUID().uuidString)"
    let defaults = try #require(UserDefaults(suiteName: suite))
    defer { defaults.removePersistentDomain(forName: suite) }
    let settings = SettingsStore(defaults: defaults, passwords: passwords)
    settings.host = "news.example.com"
    for prefix in ["s", "se", "sec", "secr", "secre", "secret"] { settings.password = prefix }
    #expect(passwords.accounts.isEmpty)
    #expect(await eventually(timeout: .seconds(3)) { !passwords.accounts.isEmpty })
    #expect(passwords.password(for: NNTPAccount(server: "news.example.com", port: 563, username: "")) == "secret")
  }

  @Test("Engine settings carry every setting the engine needs")
  func engineSettings() {
    let settings = SettingsStore.preview(host: " news.example.com ", username: "zeph", password: "secret")
    settings.connections = 12
    settings.verifyCertificate = false
    settings.deleteArchivesAfterExtracting = true
    settings.downloadAllRecoveryUpFront = true
    settings.limitsSpeed = true
    settings.speedLimitMegabytesPerSecond = 3
    let engine = settings.engineSettings
    #expect(engine.server.host == "news.example.com")
    #expect(engine.server.connections == 12 && !engine.server.verifyCertificate && engine.server.username == "zeph")
    #expect(engine.password == "secret")
    #expect(engine.processing.deleteArchivesAfterExtracting)
    #expect(engine.advanced.downloadAllRecoveryUpFront)
    #expect(engine.speedLimitBytesPerSecond == 3_000_000)
  }

  @Test("Importing the CLI's settings fills the server, account and processing")
  func importing() async throws {
    try await withDefaults { defaults in
      let passwords = InMemoryPasswordStore()
      let settings = SettingsStore(defaults: defaults, passwords: passwords)
      let imported = ImportedSettings(
        server: ServerSettings(
          host: "news.example.com", port: 443, useSSL: true, verifyCertificate: false, username: "zeph", connections: 50, retryAttempts: 4),
        password: "secret",
        processing: ProcessingSettings(repairWithPar2: false),
        advanced: AdvancedSettings(downloadAllRecoveryUpFront: true, flushFilesWhenFinished: true))
      settings.apply(imported)
      #expect(settings.host == "news.example.com" && settings.port == 443 && settings.portEdited)
      #expect(settings.username == "zeph" && settings.password == "secret")
      #expect(settings.connections == 50 && settings.retryAttempts == 4 && !settings.verifyCertificate)
      #expect(!settings.repairWithPar2 && settings.downloadAllRecoveryUpFront && settings.flushFilesWhenFinished)
      await settings.keychainSettled()
      #expect(passwords.password(for: NNTPAccount(server: "news.example.com", port: 443, username: "zeph")) == "secret")
    }
  }

  @Test("Reset All Settings brings back the defaults and removes the password")
  func reset() async throws {
    try await withDefaults { defaults in
      let passwords = InMemoryPasswordStore()
      let settings = SettingsStore(defaults: defaults, passwords: passwords)
      settings.host = "news.example.com"
      settings.password = "secret"
      settings.port = 443
      settings.extractArchives = false
      settings.retention = .whenAppQuits
      settings.savePasswordNow()
      settings.resetAll()
      await settings.keychainSettled()
      #expect(settings.host.isEmpty && settings.password.isEmpty && settings.port == 563 && !settings.portEdited)
      #expect(settings.extractArchives && settings.retention == .manually)
      #expect(passwords.accounts.isEmpty)
      let reread = SettingsStore(defaults: defaults, passwords: passwords)
      #expect(reread.host.isEmpty && reread.port == 563 && reread.extractArchives)
    }
  }

  @Test("Launch arguments in the defaults read as YES and NO")
  func argumentDomainBooleans() throws {
    try withDefaults { defaults in
      defaults.set("NO", forKey: SettingsStore.Key.startAutomatically)
      defaults.set("YES", forKey: SettingsStore.Key.showInMenuBar)
      let settings = SettingsStore(defaults: defaults, passwords: InMemoryPasswordStore())
      #expect(!settings.startAutomatically && settings.showInMenuBar)
    }
  }

  @Test("A chosen download folder survives a relaunch", .enabled(if: ProcessInfo.processInfo.environment["CI"] == nil))
  func downloadFolder() throws {
    let scratch = Scratch()
    let folder = scratch.folder("Chosen")
    try withDefaults { defaults in
      let settings = SettingsStore(defaults: defaults, passwords: InMemoryPasswordStore())
      try settings.setDownloadFolder(folder)
      #expect(settings.downloadFolder == folder)
      let reread = SettingsStore(defaults: defaults, passwords: InMemoryPasswordStore())
      #expect(reread.downloadFolder.standardizedFileURL.resolvingSymlinksInPath().path == folder.standardizedFileURL.resolvingSymlinksInPath().path)
      reread.useDefaultDownloadFolder()
      #expect(SettingsStore(defaults: defaults, passwords: InMemoryPasswordStore()).downloadFolder == AppPaths.defaultDownloadFolder)
    }
  }

  @Test("The keychain round-trips an internet password", .enabled(if: ProcessInfo.processInfo.environment["DLNZB_KEYCHAIN_TESTS"] != nil))
  func keychain() throws {
    let keychain = Keychain()
    let account = NNTPAccount(server: "dl-nzb-test-\(UUID().uuidString).invalid", port: 563, username: "tester")
    defer { keychain.deletePassword(for: account) }
    #expect(keychain.password(for: account) == nil)
    try keychain.setPassword("first", for: account)
    #expect(keychain.password(for: account) == "first")
    try keychain.setPassword("second", for: account)
    #expect(keychain.password(for: account) == "second")
    keychain.deletePassword(for: account)
    #expect(keychain.password(for: account) == nil)
  }
}

/// A password store that takes a moment to answer, and notes whether it was
/// asked on the main thread.
final class SlowPasswordStore: PasswordStore {
  private let store = InMemoryPasswordStore()
  private let mainThreadRead = Mutex<Bool?>(nil)

  var readOnMainThread: Bool? { mainThreadRead.withLock { $0 } }

  func password(for account: NNTPAccount) -> String? {
    mainThreadRead.withLock { $0 = ($0 ?? false) || Thread.isMainThread }
    Thread.sleep(forTimeInterval: 0.05)
    return store.password(for: account)
  }

  func setPassword(_ password: String, for account: NNTPAccount) throws {
    try store.setPassword(password, for: account)
  }

  func deletePassword(for account: NNTPAccount) {
    store.deletePassword(for: account)
  }
}
