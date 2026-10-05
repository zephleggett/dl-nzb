import Foundation
import Observation
import Synchronization

/// The result of opening an NZB.
public enum AddResult: Sendable, Equatable {
  case added(DownloadItem.ID)
  /// Already in the list, or its folder already exists. The app asks
  /// "Download Again" (`DownloadQueue.addAgain`) or "Show in Finder" (`folder`).
  case duplicate(DuplicateNZB)
  case failed(fileName: String, message: String)
}

/// An NZB that was opened again. It carries the file's bytes, so Download
/// Again works after the open event's security-scoped access is over.
///
/// While the earlier copy is still in the list and not finished, its folder
/// may not exist yet: the app offers Show in List (`DownloadQueue.listedItem(for:)`)
/// rather than Show in Finder.
public struct DuplicateNZB: Sendable, Equatable, Identifiable {
  public let id: UUID
  public let title: String
  /// The item in the list with the same NZB, when that is why. An
  /// unfinished one is preferred over a finished one.
  public let existingItemID: DownloadItem.ID?
  /// Where it was (or is being) downloaded, for Show in Finder.
  public let folder: URL
  let data: Data
  let fileName: String

  init(title: String, existingItemID: DownloadItem.ID?, folder: URL, data: Data, fileName: String) {
    self.id = UUID()
    self.title = title
    self.existingItemID = existingItemID
    self.folder = folder
    self.data = data
    self.fileName = fileName
  }
}

/// The list of downloads and the turn-taking between them.
///
/// One job at a time is in its network phases (connecting, checking,
/// downloading); the next one starts as soon as the current one moves on to
/// verifying, so post-processing overlaps the next download. Order is the
/// list's order, which the user can change.
///
/// Engine events arrive on each job's `AsyncStream` and are applied here on
/// the main actor. The list is saved to queue.json shortly after each change
/// and restored at launch; a job that was running then continues from its
/// folder, as the engine keeps a sidecar there.
@MainActor
@Observable
public final class DownloadQueue {
  public private(set) var items: [DownloadItem] = []
  /// Pause All is on, or a server problem paused everything. Nothing starts
  /// by itself; an item the user resumes still runs.
  public private(set) var isPaused = false
  /// Why the queue paused itself, for an alert with Open Settings, Try Again
  /// and Not Now: the server rejected the login, or could not be reached.
  /// Not Now (`dismissServerProblem`) hides the alert; the problem itself
  /// stays in `unresolvedServerProblem`.
  public private(set) var serverProblem: EngineError?
  /// The server problem that paused the queue, until Try Again
  /// (`retryServer`), Resume All, or server settings the user has finished
  /// changing (`serverSettingsCommitted`) clear it. It outlives the alert, so
  /// the window can say why nothing downloads.
  public private(set) var unresolvedServerProblem: EngineError?
  /// The last file operation that failed (Move to Trash), for an alert.
  public var actionError: String?

  public let settings: SettingsStore
  @ObservationIgnored public let engine: any DownloadEngine
  @ObservationIgnored public let storage: QueueStorage
  /// Called on the main actor when an item finishes, fails or needs
  /// attention: one notification per job.
  @ObservationIgnored public var onItemFinished: (@MainActor (DownloadItem) -> Void)?
  /// The clock for finish dates and retention; tests move it.
  @ObservationIgnored public var now: () -> Date = { Date() }

  @ObservationIgnored private let isLive: Bool
  /// Restored and allowed to start jobs (after the engine has its settings).
  @ObservationIgnored private var isActive = false
  @ObservationIgnored private var restored = false
  /// Whether Pause All was on before a server problem paused the queue, so
  /// Try Again leaves it as the user had it.
  @ObservationIgnored private var pausedBeforeServerProblem = false
  /// Counts changes to the server settings and password that reached the
  /// engine. A job remembers the count it started under, so a login refused
  /// with settings since replaced is not reported as a problem.
  @ObservationIgnored private var serverSettingsGeneration = 0
  /// The count when the current server problem paused the queue.
  @ObservationIgnored private var serverProblemGeneration = 0
  @ObservationIgnored private var sessionGenerations: [UUID: Int] = [:]
  @ObservationIgnored private var sessions: [UUID: JobSession] = [:]
  @ObservationIgnored private var intents: [UUID: Intent] = [:]
  /// Asked the engine for a session, not yet given one.
  @ObservationIgnored private var starting: Set<UUID> = []
  /// Stopped while in a network phase; the slot is theirs until they finish.
  @ObservationIgnored private var releasing: Set<UUID> = []
  /// The security-scoped download folder each running job writes in, held
  /// from start to finish (see `holdFolderAccess`).
  @ObservationIgnored private var folderAccess: [UUID: URL] = [:]
  @ObservationIgnored private var saveTask: Task<Void, Never>?
  @ObservationIgnored private var lastSave = Date.distantPast
  @ObservationIgnored private var saveGeneration = 0
  @ObservationIgnored private let writer: QueueWriter
  @ObservationIgnored private var retentionTask: Task<Void, Never>?

  /// Why the queue stopped a job itself, so its `.stopped` summary lands in
  /// the right state.
  private enum Intent {
    /// Paused before it reached the download: start again later.
    case pause
    case cancel(deleteData: Bool)
    /// Already gone from the list; deal with the folder afterwards as asked.
    case remove(FolderFate, folder: URL)
    /// The app is quitting: continue next launch.
    case quit
  }

  /// `isLive` false makes a store with fixed contents: no engine, no disk,
  /// and every action a no-op. Previews use it.
  public init(engine: any DownloadEngine, settings: SettingsStore, storage: QueueStorage = .standard, isLive: Bool = true) {
    self.engine = engine
    self.settings = settings
    self.storage = storage
    self.isLive = isLive
    self.writer = QueueWriter(storage: storage)
  }

  // MARK: Lookup

  public func item(_ id: DownloadItem.ID) -> DownloadItem? {
    items.first { $0.id == id }
  }

  private func index(of id: UUID) -> Int? {
    items.firstIndex { $0.id == id }
  }

  // MARK: Aggregates for the Dock, the menu bar and the window subtitle

  /// Items running in any phase.
  public var activeCount: Int { items.count(where: \.isRunning) }

  /// Items in a network phase: at most one, by design.
  public var downloadingCount: Int { items.count(where: \.usesNetwork) }

  public var queuedCount: Int { items.count(where: \.isQueued) }

  public var pausedCount: Int { items.count(where: \.isPaused) }

  /// Items quitting would interrupt.
  public var unfinishedCount: Int { items.count(where: \.isUnfinished) }

  /// Bytes a second across every transfer.
  public var aggregateSpeed: Double {
    items.reduce(0) { total, item in
      guard item.phase?.isTransfer == true, let speed = item.progress?.speedBytesPerSecond, speed.isFinite else { return total }
      return total + max(speed, 0)
    }
  }

  /// The share of the current batch (running, queued and paused items,
  /// weighted by size) that has downloaded. Nil when nothing is running, which
  /// is when the Dock tile and Finder progress go away.
  public var overallFraction: Double? {
    let batch = items.filter { $0.isRunning || $0.isQueued || $0.isPaused }
    guard batch.contains(where: \.isRunning) else { return nil }
    let weights = batch.map { Double(max($0.totalBytes, 1)) }
    let total = weights.reduce(0, +)
    let done = zip(batch, weights).reduce(0) { $0 + $1.0.downloadFraction * $1.1 }
    return total > 0 ? min(max(done / total, 0), 1) : 0
  }

  /// The item downloading now, or else the first one being processed.
  public var currentItem: DownloadItem? {
    items.first(where: \.usesNetwork) ?? items.first(where: \.isRunning)
  }

  /// Whether the toolbar's Pause All should read Resume All.
  public var prefersResumeAll: Bool {
    isPaused || (activeCount == 0 && pausedCount > 0)
  }

  /// Pause All would change something: an item is downloading or waiting,
  /// and the queue is not held already. Off once everything has finished,
  /// and while only post-processing runs, which cannot pause.
  public var canPauseAll: Bool {
    !isPaused && items.contains(where: \.canPause)
  }

  /// Resume All would change something: the queue is held, or an item is paused.
  public var canResumeAll: Bool {
    isPaused || pausedCount > 0
  }

  /// Waiting items won't start until the user presses Start.
  public func awaitsStart(_ item: DownloadItem) -> Bool {
    item.isQueued && !settings.startAutomatically && !item.startRequested
  }

  /// A waiting item the queue is holding back: a server problem paused
  /// everything, or Pause All is on and the user has not started this one.
  /// Its row reads "Paused" rather than "Waiting" (`StatusText.line(for:held:)`).
  public func isHeld(_ item: DownloadItem) -> Bool {
    guard item.isQueued else { return false }
    return unresolvedServerProblem != nil || (isPaused && !item.startRequested)
  }

  /// The unfinished item in the list that a duplicate is a copy of, for Show
  /// in List; nil when the earlier copy has finished (its folder is there to
  /// show in Finder) or is not in the list at all.
  public func listedItem(for duplicate: DuplicateNZB) -> DownloadItem? {
    guard let id = duplicate.existingItemID, let item = item(id), !item.isFinished else { return nil }
    return item
  }

  /// Downloads start by themselves: the setting is on and Pause All is off.
  private var startsAutomatically: Bool {
    settings.startAutomatically && !isPaused
  }

  /// A job holds server connections, or one waiting will take them when its
  /// turn comes: what the iPhone's cellular question guards.
  public var hasNetworkWork: Bool {
    let automatic = startsAutomatically
    return items.contains { $0.usesNetwork || $0.mayStart(automatically: automatic) }
  }

  // MARK: Launch and quit

  /// Reads queue.json. Interrupted jobs come back queued and continue from
  /// their folders when they start; retention is applied. Idempotent.
  public func restore() {
    guard isLive, !restored else { return }
    restored = true
    let snapshot = storage.load()
    items = snapshot.items.map(restoredItem)
    isPaused = snapshot.pausedByUser
    if settings.retention == .whenAppQuits {
      removeFinished { _ in true }
    }
    applyRetention()
    Log.queue.info("restored \(self.items.count) downloads, \(self.unfinishedCount) unfinished")
  }

  private func restoredItem(_ stored: DownloadItem) -> DownloadItem {
    var item = stored.rebasedIntoCurrentContainer()
    // The queue's copy is always named after the item; an older list may
    // remember it somewhere it no longer is.
    let copy = storage.nzbURL(for: item.id)
    if !FileManager.default.fileExists(atPath: item.nzbURL.path(percentEncoded: false)),
      FileManager.default.fileExists(atPath: copy.path(percentEncoded: false))
    {
      item.nzbURL = copy
    }
    switch item.state {
    case .running, .queued:
      item.state = .queued
      // Interrupted downloads wait for Start like any other when downloads
      // do not start automatically.
      item.startRequested = false
    default:
      break
    }
    if item.isUnfinished && !FileManager.default.fileExists(atPath: item.nzbURL.path(percentEncoded: false)) {
      item.state = .failed(message: "The NZB file for this download is missing.", resumable: false)
    }
    return item
  }

  /// Lets the scheduler start jobs, once the engine has its settings.
  public func activate() {
    guard isLive, !isActive else { return }
    isActive = true
    observeSettings()
    retentionTask = Task { [weak self] in
      while !Task.isCancelled {
        try? await Task.sleep(for: .seconds(15 * 60))
        self?.applyRetention()
      }
    }
    schedule()
  }

  /// Stops every job so it can continue next launch, applies "When dl-nzb
  /// quits" retention and saves the list. The app awaits this before quitting,
  /// then shuts the engine down.
  public func prepareForQuit(timeout: Duration = .seconds(5)) async {
    guard isLive else { return }
    isActive = false
    retentionTask?.cancel()
    for id in Set(sessions.keys).union(starting) where intents[id] == nil {
      intents[id] = item(id)?.isPaused == true ? .pause : .quit
      sessions[id]?.stop()
    }
    let deadline = ContinuousClock.now + timeout
    while !(sessions.isEmpty && starting.isEmpty), ContinuousClock.now < deadline {
      try? await Task.sleep(for: .milliseconds(20))
    }
    if settings.retention == .whenAppQuits {
      removeFinished { _ in true }
    }
    saveNow()
  }

  // MARK: Adding

  /// Opens NZBs from Finder, the Dock, an open panel, Files or the share
  /// sheet. Each is copied into the queue's folder first (taking care of
  /// security-scoped URLs), so the original can go away.
  public func add(urls: [URL]) async -> [AddResult] {
    var results: [AddResult] = []
    for url in urls {
      results.append(await add(url))
    }
    return results
  }

  public func add(_ url: URL) async -> AddResult {
    let fileName = url.lastPathComponent
    guard isLive else { return .failed(fileName: fileName, message: "Downloads are not available here.") }
    do {
      let data = try await NzbImport.read(url)
      return await add(data: data, fileName: fileName, allowingDuplicate: false)
    } catch let error as EngineError {
      return .failed(fileName: fileName, message: error.message)
    } catch {
      return .failed(fileName: fileName, message: "dl-nzb could not read \(fileName).")
    }
  }

  /// Download Again: adds the duplicate under a fresh folder name.
  public func addAgain(_ duplicate: DuplicateNZB) async -> AddResult {
    await add(data: duplicate.data, fileName: duplicate.fileName, allowingDuplicate: true)
  }

  private func add(data: Data, fileName: String, allowingDuplicate: Bool) async -> AddResult {
    guard isLive else { return .failed(fileName: fileName, message: "Downloads are not available here.") }
    let fingerprint = NzbFingerprint.of(data)
    if !allowingDuplicate, let duplicate = listedDuplicate(fingerprint: fingerprint, data: data, fileName: fileName) {
      return .duplicate(duplicate)
    }
    let id = UUID()
    let storage = storage
    let copy: URL
    do {
      copy = try await Task.detached(priority: .userInitiated) { try storage.storeNZB(data, for: id) }.value
    } catch {
      Log.queue.error("could not copy \(fileName, privacy: .public) into the queue: \(error.localizedDescription, privacy: .public)")
      return .failed(fileName: fileName, message: "dl-nzb could not store a copy of \(fileName).")
    }
    let info: NzbInfo
    do {
      info = try await engine.inspect(copy)
    } catch {
      storage.removeNZB(for: id)
      let message = (error as? EngineError)?.message ?? "\(fileName) is not a valid NZB file."
      return .failed(fileName: fileName, message: message)
    }

    let stem = (fileName as NSString).deletingPathExtension
    // The engine falls back to the file's name, which for the copy is the id.
    let title = ReleaseName.best([info.title == id.uuidString ? nil : info.title, stem], fallback: stem)
    let base = settings.downloadFolder
    // Checked again after the awaits: another add may have got there first.
    if !allowingDuplicate {
      if let duplicate = listedDuplicate(fingerprint: fingerprint, data: data, fileName: fileName) {
        storage.removeNZB(for: id)
        return .duplicate(duplicate)
      }
      let plain = base.appending(path: ReleaseName.folderName(title), directoryHint: .isDirectory)
      if !isListed(plain) && Self.holdsFiles(plain) {
        storage.removeNZB(for: id)
        return .duplicate(DuplicateNZB(title: title, existingItemID: nil, folder: plain, data: data, fileName: fileName))
      }
    }

    let folder = uniqueFolder(for: title, in: base)
    let item = DownloadItem(
      id: id, title: title, nzbURL: copy, originalFileName: fileName, fingerprint: fingerprint, outputDirectory: folder.url,
      addedAt: now(), info: info, copyNumber: folder.number)
    items.append(item)
    Log.queue.info("added \(title, privacy: .public)")
    scheduleSave()
    schedule()
    return .added(id)
  }

  /// The listed item with the same NZB: one still to finish if there is
  /// one (that is the copy the user is likely thinking of), else the latest.
  private func listedDuplicate(fingerprint: String, data: Data, fileName: String) -> DuplicateNZB? {
    let matches = items.filter { $0.fingerprint == fingerprint }
    guard let existing = matches.first(where: { !$0.isFinished }) ?? matches.last else { return nil }
    return DuplicateNZB(title: existing.title, existingItemID: existing.id, folder: existing.outputDirectory, data: data, fileName: fileName)
  }

  /// "Name", or "Name 2", "Name 3" … when an item in the list or a folder on
  /// disk has it already; with the number, nil for the first.
  private func uniqueFolder(for title: String, in base: URL) -> (url: URL, number: Int?) {
    let name = ReleaseName.folderName(title)
    var candidate = base.appending(path: name, directoryHint: .isDirectory)
    var number = 1
    while isListed(candidate) || FileManager.default.fileExists(atPath: candidate.path(percentEncoded: false)) {
      number += 1
      candidate = base.appending(path: "\(name) \(number)", directoryHint: .isDirectory)
    }
    return (candidate, number > 1 ? number : nil)
  }

  private func isListed(_ folder: URL) -> Bool {
    let path = Self.normalisedPath(folder)
    return items.contains { Self.normalisedPath($0.outputDirectory) == path }
  }

  private static func normalisedPath(_ url: URL) -> String {
    var path = url.standardizedFileURL.path(percentEncoded: false)
    while path.count > 1 && path.hasSuffix("/") { path.removeLast() }
    return path
  }

  /// A folder with anything in it: an earlier download of the same release.
  private static func holdsFiles(_ folder: URL) -> Bool {
    let contents = try? FileManager.default.contentsOfDirectory(atPath: folder.path(percentEncoded: false))
    return !(contents ?? []).isEmpty
  }

  // MARK: Controls

  /// Starts a waiting item when downloads do not start automatically, or
  /// retries a stopped or failed one.
  public func start(_ id: DownloadItem.ID) {
    guard isLive, let index = index(of: id) else { return }
    if items[index].canRetry {
      retry(id)
      return
    }
    guard items[index].isQueued else { return }
    items[index].startRequested = true
    scheduleSave()
    schedule()
  }

  /// Start for every waiting item.
  public func startAll() {
    guard isLive else { return }
    for index in items.indices where items[index].isQueued {
      items[index].startRequested = true
    }
    scheduleSave()
    schedule()
  }

  /// A waiting item stays where it is; a download releases its connections
  /// and waits; a job still connecting or checking starts over later. Jobs in
  /// post-processing cannot pause.
  public func pause(_ id: DownloadItem.ID) {
    guard isLive, let index = index(of: id) else { return }
    switch items[index].state {
    case .queued:
      items[index].state = .paused
    case .running(let phase) where phase.isTransfer && sessions[id] != nil:
      sessions[id]?.pause()
      items[index].state = .paused
    case .running(let phase) where phase.usesNetwork:
      // Connecting or checking: stopped now, started over on resume. A job
      // still starting is stopped as soon as its session arrives.
      items[index].state = .paused
      interrupt(id, holdingSlot: true, for: .pause)
    default:
      return
    }
    scheduleSave()
    schedule()
  }

  /// Back in line, and allowed to run even when downloads do not start
  /// automatically or Pause All is on: the user asked for it.
  public func resume(_ id: DownloadItem.ID) {
    guard isLive, let index = index(of: id), items[index].isPaused else { return }
    items[index].state = .queued
    items[index].startRequested = true
    scheduleSave()
    schedule()
  }

  /// Pauses what is downloading and holds back everything waiting.
  public func pauseAll() {
    guard isLive else { return }
    isPaused = true
    for index in items.indices where items[index].isQueued {
      items[index].startRequested = false
    }
    for item in items where item.usesNetwork {
      pause(item.id)
    }
    scheduleSave()
  }

  /// Resumes every paused item, lifts the hold, and clears a server problem
  /// so the queue tries the server again.
  public func resumeAll() {
    resumeAll(keepingPaused: [])
  }

  /// Resume All, except for the items in `kept`, which stay paused. The
  /// iPhone pauses everything itself (on cellular, or when the system ends
  /// its background time) and uses this to undo exactly that, leaving alone
  /// whatever the user had paused one by one before.
  public func resumeAll(keepingPaused kept: Set<DownloadItem.ID>) {
    guard isLive else { return }
    isPaused = false
    serverProblem = nil
    unresolvedServerProblem = nil
    for index in items.indices where items[index].isPaused && !kept.contains(items[index].id) {
      items[index].state = .queued
      items[index].startRequested = true
    }
    scheduleSave()
    schedule()
  }

  /// Stop. The data stays, so Retry continues, unless `deletingData`.
  public func stop(_ id: DownloadItem.ID, deletingData: Bool = false) {
    guard isLive, let index = index(of: id), items[index].canStop else { return }
    let item = items[index]
    items[index].state = .stopped
    if !interrupt(id, holdingSlot: item.usesNetwork, for: .cancel(deleteData: deletingData)) {
      if deletingData {
        items[index].progress = nil
        items[index].earlierRunSeconds = nil
        deleteFolder(item.outputDirectory)
      } else {
        removeFolderIfEmpty(item.outputDirectory)
      }
    }
    Log.queue.info("stopped \(item.title, privacy: .public)\(deletingData ? " and deleted its data" : "", privacy: .public)")
    scheduleSave()
    schedule()
  }

  /// Back in line to continue (or start over, if its data was deleted).
  public func retry(_ id: DownloadItem.ID) {
    guard isLive, let index = index(of: id), items[index].canRetry else { return }
    items[index].state = .queued
    items[index].startRequested = true
    scheduleSave()
    schedule()
  }

  /// Takes the item off the list; a running job stops. Its files stay,
  /// unless `deletingData`, which deletes the folder of an item that has not
  /// finished (what it downloaded so far). A finished download's files always
  /// stay. Ask first when `DownloadItem.needsRemovalConfirmation`.
  public func remove(_ id: DownloadItem.ID, deletingData: Bool = false) {
    let unfinished = item(id).map { !$0.isFinished } ?? false
    detach(id, folder: deletingData && unfinished ? .delete : .keep)
  }

  /// Takes the item off the list and moves its folder to the Trash (on the
  /// Mac; iPhone and iPad have no Trash, so the folder is deleted).
  public func moveToTrash(_ id: DownloadItem.ID) {
    detach(id, folder: .trash)
  }

  /// What becomes of a removed item's folder.
  private enum FolderFate {
    /// Left alone, unless the job never got as far as putting anything in it.
    case keep
    case trash
    case delete
  }

  private func detach(_ id: UUID, folder fate: FolderFate) {
    guard isLive, let index = index(of: id) else { return }
    let item = items.remove(at: index)
    if !interrupt(id, holdingSlot: item.usesNetwork, for: .remove(fate, folder: item.outputDirectory)) {
      dispose(item.outputDirectory, fate)
    }
    storage.removeNZB(for: id)
    let detail =
      switch fate {
      case .keep: ""
      case .trash: " and moved its folder to the Trash"
      case .delete: " and deleted its data"
      }
    Log.queue.info("removed \(item.title, privacy: .public)\(detail, privacy: .public)")
    scheduleSave()
    schedule()
  }

  private func dispose(_ folder: URL, _ fate: FolderFate) {
    switch fate {
    case .keep: removeFolderIfEmpty(folder)
    case .trash: trashFolder(folder)
    case .delete: deleteFolder(folder)
    }
  }

  /// Drag to reorder: the order is the order downloads take turns in.
  public func move(fromOffsets source: IndexSet, toOffset destination: Int) {
    guard isLive else { return }
    items.moveElements(fromOffsets: source, toOffset: destination)
    scheduleSave()
  }

  /// Download Anyway, after the pre-flight scan found too much missing: no
  /// scan this time, and whatever is there is downloaded.
  public func downloadAnyway(_ id: DownloadItem.ID) {
    guard isLive, let index = index(of: id), case .needsAttention(.unrepairable) = items[index].state else { return }
    items[index].downloadAnyway = true
    items[index].state = .queued
    items[index].startRequested = true
    scheduleSave()
    schedule()
  }

  /// The archive's password: post-processing runs again with it. The
  /// download itself is done, so this does not wait for a turn.
  public func providePassword(_ id: DownloadItem.ID, password: String) {
    guard isLive, let index = index(of: id), case .needsAttention(.password) = items[index].state, !password.isEmpty else { return }
    items[index].passwords.append(password)
    items[index].state = .running(.extracting)
    let item = items[index]
    let passwords = (item.info?.passwords ?? []) + item.passwords
    let engine = engine
    openSession(for: id, folder: item.outputDirectory) {
      try await engine.reprocess(directory: item.outputDirectory, passwords: passwords)
    }
    scheduleSave()
  }

  /// Not Now: hides the server problem's alert. The queue stays paused, and
  /// `unresolvedServerProblem` still says why, until Try Again, Resume All or
  /// new server settings.
  public func dismissServerProblem() {
    serverProblem = nil
  }

  /// Try Again after a server problem: the hold lifts and waiting items take
  /// their turns, with whatever settings the engine has now. Pause All stays
  /// on if the user had turned it on before, and items the user paused stay
  /// paused.
  public func retryServer() {
    guard isLive, unresolvedServerProblem != nil else { return }
    Log.queue.info("trying the server again")
    serverProblem = nil
    unresolvedServerProblem = nil
    isPaused = pausedBeforeServerProblem
    scheduleSave()
    schedule()
  }

  /// New server settings or a new password reached the engine. Jobs started
  /// before this no longer report server problems: their login was with the
  /// old ones. The queue does not try again yet (the user may be typing
  /// still); `serverSettingsCommitted` does.
  public func serverSettingsChanged() {
    serverSettingsGeneration += 1
  }

  /// The user has finished with the server settings (closed them, or Test
  /// Connection worked): if a server problem paused the queue and the
  /// settings changed since, try again with the new ones.
  public func serverSettingsCommitted() {
    guard unresolvedServerProblem != nil, serverSettingsGeneration > serverProblemGeneration else { return }
    Log.queue.info("server settings changed; trying again after the server problem")
    retryServer()
  }

  /// "After one day": finished items older than a day leave the list. Their
  /// files stay. Runs at launch, every quarter of an hour and on demand.
  public func applyRetention() {
    guard isLive, settings.retention == .afterOneDay else { return }
    let cutoff = now().addingTimeInterval(-24 * 60 * 60)
    removeFinished { ($0.finishedAt ?? .distantFuture) <= cutoff }
  }

  /// Clears every finished item from the list (Remove Finished).
  public func removeAllFinished() {
    guard isLive else { return }
    removeFinished { _ in true }
  }

  private func removeFinished(where predicate: (DownloadItem) -> Bool) {
    let leaving = items.filter { $0.isFinished && predicate($0) }
    guard !leaving.isEmpty else { return }
    let ids = Set(leaving.map(\.id))
    items.removeAll { ids.contains($0.id) }
    for id in ids { storage.removeNZB(for: id) }
    scheduleSave()
  }

  // MARK: Folder access

  /// On the Mac the download folder is a security-scoped bookmark that
  /// `SettingsStore` keeps open while it is the chosen folder. The engine
  /// writes into a job's folder for minutes or hours, by plain path, so each
  /// job takes its own reference on that access for its whole run: choosing
  /// another folder meanwhile (which ends the store's) leaves running jobs
  /// able to finish. A job folder outside the current download folder (one
  /// chosen before a change, restored after relaunch) has no scoped URL to
  /// hold; on iPhone and iPad nothing is scoped. Both are no-ops.
  private func holdFolderAccess(for id: UUID, folder: URL) {
    guard folderAccess[id] == nil else { return }
    let base = settings.downloadFolder
    let basePath = Self.normalisedPath(base)
    let folderPath = Self.normalisedPath(folder)
    guard folderPath == basePath || folderPath.hasPrefix(basePath.hasSuffix("/") ? basePath : basePath + "/") else { return }
    if base.startAccessingSecurityScopedResource() {
      folderAccess[id] = base
    }
  }

  private func releaseFolderAccess(for id: UUID) {
    folderAccess.removeValue(forKey: id)?.stopAccessingSecurityScopedResource()
  }

  // MARK: Scheduler

  /// Starts the next waiting item if the network slot is free.
  private func schedule() {
    // A server problem holds everything, even items the user started.
    guard isLive, isActive, unresolvedServerProblem == nil, settings.hasServer else { return }
    // A job starting is already `.running(.connecting)`; one paused, cancelled
    // or removed while connecting holds the slot until the engine lets go.
    let slotTaken = !releasing.isEmpty || items.contains(where: \.usesNetwork)
    guard !slotTaken else { return }
    let automatic = startsAutomatically
    guard let next = items.first(where: { $0.mayStart(automatically: automatic) }) else { return }
    begin(next.id)
  }

  private func begin(_ id: UUID) {
    guard let index = index(of: id) else { return }
    if let session = sessions[id] {
      // Paused mid-download and back in line: the engine still holds it.
      items[index].state = .running(items[index].progress?.phase ?? .downloading)
      session.resume()
      return
    }
    let item = items[index]
    items[index].state = .running(.connecting)
    if item.progress == nil { items[index].visitedPhases = [] }
    let request = JobRequest(
      nzbURL: item.nzbURL, outputDirectory: item.outputDirectory, passwords: item.passwords,
      preflight: item.downloadAnyway ? .never : settings.preflight, onUnrepairable: item.downloadAnyway ? .continue : .stop,
      title: item.title)
    Log.queue.info("starting \(item.title, privacy: .public)")
    sessionGenerations[id] = serverSettingsGeneration
    let engine = engine
    openSession(for: id, folder: item.outputDirectory) { try await engine.start(request) }
  }

  /// Asks the engine for a job's session, holding its folder from now until
  /// it finishes. Until the session arrives the job is `starting`.
  private func openSession(for id: UUID, folder: URL, _ open: @escaping @Sendable () async throws -> JobSession) {
    starting.insert(id)
    holdFolderAccess(for: id, folder: folder)
    Task { [weak self] in
      guard let self else { return }
      do {
        self.started(try await open(), for: id)
      } catch {
        self.startFailed(id, error)
      }
    }
  }

  /// Asks a job that is starting or running to stop, and why. False when
  /// there is no such job. One that was `holdingSlot` (in a network phase)
  /// keeps the slot until the engine lets go of its connections.
  @discardableResult
  private func interrupt(_ id: UUID, holdingSlot: Bool, for intent: Intent) -> Bool {
    guard sessions[id] != nil || starting.contains(id) else { return false }
    intents[id] = intent
    if holdingSlot { releasing.insert(id) }
    sessions[id]?.stop()
    return true
  }

  private func started(_ session: JobSession, for id: UUID) {
    starting.remove(id)
    sessions[id] = session
    Task { [weak self] in
      for await event in session.events {
        self?.apply(event, to: id)
      }
      self?.sessionEnded(id)
    }
    // Paused, cancelled or removed while it was starting, or the app is quitting.
    if intents[id] != nil || !isActive {
      if intents[id] == nil { intents[id] = .quit }
      if item(id)?.usesNetwork == true || index(of: id) == nil { releasing.insert(id) }
      session.stop()
    }
  }

  private func startFailed(_ id: UUID, _ error: any Error) {
    let engineError = error as? EngineError ?? EngineError(.io, error.localizedDescription)
    finishFailed(id, engineError.message, kind: engineError.kind)
  }

  /// The stream ended without `.finished`: an engine fault.
  private func sessionEnded(_ id: UUID) {
    guard sessions[id] != nil else { return }
    finishFailed(id, "The download stopped unexpectedly.")
  }

  /// A failure the engine did not summarise itself; the job can continue.
  private func finishFailed(_ id: UUID, _ message: String, kind: EngineError.Kind? = nil) {
    let folder = item(id)?.outputDirectory ?? storage.directory
    finish(id, JobSummary(outcome: .failed, message: message, errorKind: kind, outputDirectory: folder, resumable: true))
  }

  // MARK: Events

  /// Applies one engine event to its item.
  func apply(_ event: JobEvent, to id: UUID) {
    if case .finished(let summary) = event {
      finish(id, summary)
      return
    }
    guard let index = index(of: id) else { return }
    switch event {
    case .phase(let phase):
      note(phase, at: index)
      switch items[index].state {
      case .running:
        items[index].state = .running(phase)
      case .paused where !phase.isTransfer && intents[id] == nil:
        // The pause came as the download ended; the engine carried on.
        items[index].state = .running(phase)
      default:
        break
      }
      if !phase.usesNetwork { releasing.remove(id) }
      scheduleSave()
      schedule()
    case .progress(let progress):
      items[index].progress = progress
      note(progress.phase, at: index)
      if case .running(let current) = items[index].state, current != progress.phase {
        items[index].state = .running(progress.phase)
      }
      saveIfDue()
    case .availability(let availability):
      items[index].availability = availability
    case .warning(let warning):
      Log.queue.notice("\(self.items[index].title, privacy: .public): \(warning, privacy: .public)")
      if items[index].warnings.count < 50 { items[index].warnings.append(warning) }
    case .finished:
      break
    }
  }

  private func note(_ phase: JobPhase, at index: Int) {
    if !items[index].visitedPhases.contains(phase) {
      items[index].visitedPhases.append(phase)
    }
  }

  private func finish(_ id: UUID, _ summary: JobSummary) {
    sessions[id] = nil
    starting.remove(id)
    releasing.remove(id)
    let intent = intents.removeValue(forKey: id)
    let generation = sessionGenerations.removeValue(forKey: id)
    defer {
      // After any trashing or deleting below, which needs the access too.
      releaseFolderAccess(for: id)
      scheduleSave()
      schedule()
    }
    guard let index = index(of: id) else {
      if case .remove(let fate, let folder) = intent {
        dispose(folder, fate)
      }
      return
    }
    var item = items[index]
    // The engine times this run only. One that continued an earlier run (after
    // quitting, Retry, a server problem, a password) finishes "in" the time of
    // them all, so the earlier runs' time is carried until the job completes.
    var summary = summary
    let totalSeconds = (item.earlierRunSeconds ?? 0) + summary.elapsedSeconds
    if intent == nil && summary.outcome.isSuccess {
      summary.elapsedSeconds = totalSeconds
      item.earlierRunSeconds = nil
    } else {
      item.earlierRunSeconds = totalSeconds
    }
    item.summary = summary
    if let availability = summary.availability { item.availability = availability }
    var notify = false
    switch intent {
    case .pause:
      item.state = .paused
    case .cancel(let deleteData):
      item.state = .stopped
      if deleteData {
        item.progress = nil
        item.earlierRunSeconds = nil
        deleteFolder(item.outputDirectory)
      } else {
        removeFolderIfEmpty(item.outputDirectory)
      }
    case .quit:
      item.state = .queued
    case .remove:
      break
    case nil:
      notify = settle(&item, with: summary, generation: generation)
    }
    items[index] = item
    if notify { onItemFinished?(item) }
  }

  /// The state a summary puts an item in. True when the user should hear
  /// about it. `generation` is the server settings' count when the job
  /// started, nil for one that never logged in (a password retry).
  private func settle(_ item: inout DownloadItem, with summary: JobSummary, generation: Int?) -> Bool {
    let title = item.title
    switch summary.outcome {
    case .completed, .completedWithIssues:
      item.state = .finished(summary)
      item.finishedAt = now()
      Log.queue.info("finished \(title, privacy: .public)")
      return true
    case .stopped:
      // Stopped by the engine itself (shutdown, the system ending a
      // background task): it can continue.
      item.state = .paused
      return false
    case .needsPassword:
      item.state = .needsAttention(.password)
      return true
    case .unrepairable:
      let availability =
        summary.availability ?? item.availability
        ?? AvailabilityInfo(
          articlesTotal: summary.articlesTotal, articlesMissing: summary.articlesFailed, missingBytes: 0, recoveryBytes: 0, verdict: .unrepairable)
      item.state = .needsAttention(.unrepairable(availability))
      return true
    case .failed:
      let message = summary.message ?? summary.errorKind?.defaultMessage ?? "The download failed."
      if let kind = summary.errorKind, kind.isServerProblem {
        item.state = .queued
        if let generation, generation < serverSettingsGeneration {
          // It logged in with settings the user has since changed: it starts
          // again with the new ones rather than raising a stale problem.
          Log.queue.info("\(title, privacy: .public) hit a server problem with old settings; starting it again")
          return false
        }
        // Every job would fail the same way: wait for the user instead.
        if unresolvedServerProblem == nil {
          pausedBeforeServerProblem = isPaused
          serverProblemGeneration = serverSettingsGeneration
        }
        let problem = EngineError(kind, message)
        serverProblem = problem
        unresolvedServerProblem = problem
        isPaused = true
        Log.queue.error("server problem, pausing the queue: \(message, privacy: .public)")
        return false
      }
      if summary.errorKind == .diskFull {
        item.state = .needsAttention(.diskFull(message))
        return true
      }
      item.state = .failed(message: message, resumable: summary.resumable)
      Log.queue.error("\(title, privacy: .public) failed: \(message, privacy: .public)")
      return true
    }
  }

  // MARK: Files

  private func deleteFolder(_ folder: URL) {
    Task.detached(priority: .utility) {
      do {
        try FileManager.default.removeItem(at: folder)
      } catch CocoaError.fileNoSuchFile {
        return
      } catch {
        Log.queue.error("could not delete \(folder.path(percentEncoded: false), privacy: .public): \(error.localizedDescription, privacy: .public)")
      }
    }
  }

  /// The engine makes a job's folder when the job starts, so one that stopped
  /// before anything arrived (a login the server refused, too much missing)
  /// would leave an empty folder behind when it is stopped or removed. Its
  /// name was free when the job was added, so nothing of the user's is in it.
  private func removeFolderIfEmpty(_ folder: URL) {
    Task.detached(priority: .utility) {
      let path = folder.path(percentEncoded: false)
      guard let contents = try? FileManager.default.contentsOfDirectory(atPath: path),
        contents.allSatisfy({ $0 == ".DS_Store" })
      else { return }
      do {
        try FileManager.default.removeItem(at: folder)
      } catch {
        Log.queue.error("could not remove the empty folder \(path, privacy: .public): \(error.localizedDescription, privacy: .public)")
      }
    }
  }

  private func trashFolder(_ folder: URL) {
    Task { [weak self] in
      let failure: String? = await Task.detached(priority: .utility) {
        guard FileManager.default.fileExists(atPath: folder.path(percentEncoded: false)) else { return nil }
        do {
          #if os(macOS)
            try FileManager.default.trashItem(at: folder, resultingItemURL: nil)
          #else
            try FileManager.default.removeItem(at: folder)
          #endif
          return nil
        } catch {
          return error.localizedDescription
        }
      }.value
      if let failure {
        Log.queue.error("could not move \(folder.lastPathComponent, privacy: .public) to the Trash: \(failure, privacy: .public)")
        self?.actionError = "“\(folder.lastPathComponent)” could not be moved to the Trash. \(failure)"
      }
    }
  }

  // MARK: Settings

  /// Start automatically turned on, a server configured, retention changed:
  /// each may let something start or leave.
  private func observeSettings() {
    withObservationTracking {
      _ = settings.startAutomatically
      _ = settings.hasServer
      _ = settings.retention
    } onChange: { [weak self] in
      Task { @MainActor in
        guard let self, self.isActive else { return }
        self.applyRetention()
        self.schedule()
        self.observeSettings()
      }
    }
  }

  // MARK: Persistence

  /// Saves shortly after the last change, so a burst of changes is one write.
  private func scheduleSave() {
    guard isLive else { return }
    saveTask?.cancel()
    saveTask = Task { [weak self] in
      try? await Task.sleep(for: .milliseconds(500))
      guard !Task.isCancelled else { return }
      self?.saveSoon()
    }
  }

  /// Progress alone saves every ten seconds at most.
  private func saveIfDue() {
    guard isLive, saveTask == nil, now().timeIntervalSince(lastSave) > 10 else { return }
    scheduleSave()
  }

  private var snapshot: QueueStorage.Snapshot {
    QueueStorage.Snapshot(items: items, pausedByUser: unresolvedServerProblem == nil ? isPaused : pausedBeforeServerProblem)
  }

  /// Encoding the whole list costs more than writing it, so both happen off
  /// the main actor; the snapshot is a copy-on-write value.
  private func saveSoon() {
    saveTask = nil
    lastSave = now()
    saveGeneration += 1
    let (snapshot, generation, writer) = (snapshot, saveGeneration, writer)
    Task.detached(priority: .utility) { writer.write(snapshot, generation: generation) }
  }

  /// Writes the list now, on this thread: for quitting.
  public func saveNow() {
    guard isLive else { return }
    saveTask?.cancel()
    saveTask = nil
    saveGeneration += 1
    writer.supersede(through: saveGeneration)
    do {
      try storage.save(snapshot)
      lastSave = now()
    } catch {
      Log.persistence.error("could not save the queue: \(error.localizedDescription, privacy: .public)")
    }
  }

  // MARK: Previews

  /// A queue with fixed items and nothing behind it. Actions are no-ops.
  public static func preview(items: [DownloadItem] = PreviewData.items, settings: SettingsStore? = nil, isPaused: Bool = false) -> DownloadQueue {
    let queue = DownloadQueue(engine: SimulatedEngine(), settings: settings ?? .preview(), storage: .temporary(), isLive: false)
    queue.items = items
    queue.isPaused = isPaused
    return queue
  }

  /// For previews of the server problem alert and notice.
  public static func preview(serverProblem: EngineError, items: [DownloadItem] = [PreviewData.queued]) -> DownloadQueue {
    let queue = preview(items: items, isPaused: true)
    queue.serverProblem = serverProblem
    queue.unresolvedServerProblem = serverProblem
    return queue
  }
}

/// Writes queue.json off the main actor, dropping a write that a newer one
/// has overtaken, so the file on disk is never older than one already written.
final class QueueWriter: Sendable {
  private let storage: QueueStorage
  private let latest = Mutex(0)

  init(storage: QueueStorage) {
    self.storage = storage
  }

  func write(_ snapshot: QueueStorage.Snapshot, generation: Int) {
    let data: Data
    do {
      data = try QueueStorage.encode(snapshot)
    } catch {
      Log.persistence.error("could not encode the queue: \(error.localizedDescription, privacy: .public)")
      return
    }
    // The lock is held across the write so two writes never interleave.
    latest.withLock { written in
      guard generation > written else { return }
      do {
        try storage.write(data)
        written = generation
      } catch {
        Log.persistence.error("could not save the queue: \(error.localizedDescription, privacy: .public)")
      }
    }
  }

  /// A synchronous save has written everything up to `generation`.
  func supersede(through generation: Int) {
    latest.withLock { $0 = max($0, generation) }
  }
}

extension Array {
  /// SwiftUI's `move(fromOffsets:toOffset:)`, which the Kit cannot import:
  /// `destination` is an offset in the array before the move.
  mutating func moveElements(fromOffsets source: IndexSet, toOffset destination: Int) {
    let moving = source.map { self[$0] }
    let movedFromBefore = source.count(in: 0..<Swift.min(destination, count))
    for index in source.reversed() { remove(at: index) }
    insert(contentsOf: moving, at: Swift.max(0, Swift.min(destination - movedFromBefore, count)))
  }
}

/// Reading an NZB the user opened, wherever it is.
enum NzbImport {
  /// NZBs are text; anything this big is something else.
  static let maximumBytes = 256 * 1024 * 1024

  /// Takes up security-scoped access (an open event, the Files app), and
  /// reads through a file coordinator so a file in iCloud Drive downloads first.
  static func read(_ url: URL) async throws -> Data {
    try await Task.detached(priority: .userInitiated) {
      let accessing = url.startAccessingSecurityScopedResource()
      defer { if accessing { url.stopAccessingSecurityScopedResource() } }
      var coordinationError: NSError?
      var result: Result<Data, any Error> = .failure(CocoaError(.fileReadUnknown))
      NSFileCoordinator().coordinate(readingItemAt: url, options: [.withoutChanges], error: &coordinationError) { readable in
        result = Result {
          let size = (try? readable.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
          guard size <= maximumBytes else { throw EngineError(.nzb, "\(url.lastPathComponent) is too large to be an NZB file.") }
          return try Data(contentsOf: readable)
        }
      }
      if let coordinationError { throw coordinationError }
      return try result.get()
    }.value
  }
}
