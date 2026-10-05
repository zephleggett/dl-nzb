import DlNzbKit
import DlNzbRust
import DlNzbUI
import Foundation
import Observation
import SwiftUI

/// The iPhone and iPad app around the Kit's `AppModel`: the background
/// runner, the cellular gate, notifications, where NZBs come in, and what the
/// screens are showing.
///
/// Every user action goes through here rather than straight to the queue, so
/// that each one passes the cellular gate and, once it runs, can start a
/// continued-processing task for the background.
@MainActor
@Observable
final class AppRuntime {
  let model: AppModel
  let phoneSettings: PhoneSettings
  let holds: QueueHolds
  let background: BackgroundRunner
  let gate: NetworkGate
  let router: OpenRouter

  /// The download shown in the detail column (iPad) or pushed (iPhone).
  var selection: DownloadItem.ID?
  var isShowingSettings = false
  var isShowingOnboarding = false
  var isShowingImporter = false

  @ObservationIgnored let notifier: Notifier
  @ObservationIgnored private let pathMonitor = PathMonitor()
  @ObservationIgnored private var thermalObserver: (any NSObjectProtocol)?
  @ObservationIgnored private var launched = false

  var queue: DownloadQueue { model.queue }
  var settings: SettingsStore { model.settings }

  init(model: AppModel? = nil) {
    let model = model ?? AppModel(makeEngine: { $0.makeEngine() })
    let holds = QueueHolds(queue: model.queue)
    let phoneSettings = PhoneSettings()
    self.model = model
    self.phoneSettings = phoneSettings
    self.holds = holds
    self.background = BackgroundRunner(
      queue: model.queue, holds: holds, scheduler: SystemContinuedTaskScheduler(), backgroundTime: SystemBackgroundTime())
    self.gate = NetworkGate(queue: model.queue, holds: holds, allowsCellular: { phoneSettings.allowsCellular })
    self.router = OpenRouter { urls in await model.open(urls) }
    self.notifier = Notifier()
  }

  /// Once, when the window first appears.
  func launch() {
    guard !launched else { return }
    launched = true
    model.launch()
    queue.onItemFinished = { [weak self] item in self?.itemFinished(item) }
    notifier.onOpenItem = { [weak self] id in self?.selection = id }
    gate.onActionsRan = { [weak self] in self?.background.userStartedWork() }
    background.onPausedInBackground = { [weak self] in self?.pausedInBackground() }
    pathMonitor.start { [weak self] expensive in
      #if DEBUG
        // `-simulateCellular YES`: the simulator is always on an ordinary network.
        let expensive = expensive || UserDefaults.standard.bool(forKey: "simulateCellular")
      #endif
      self?.gate.networkChanged(isExpensive: expensive)
    }
    thermalObserver = NotificationCenter.default.addObserver(forName: ProcessInfo.thermalStateDidChangeNotification, object: nil, queue: nil) { _ in
      let state = ProcessInfo.processInfo.thermalState
      AppLog.background.notice("thermal state is now \(state.logName, privacy: .public)")
    }
    if !settings.hasServer {
      isShowingOnboarding = true
    }
    #if DEBUG
      importCLIConfigForTesting()
      openOnLaunchForTesting()
    #endif
  }

  // MARK: Scene

  func scenePhaseChanged(to phase: ScenePhase) {
    // The queue must be restored before the background runner releases a
    // hold on it, or the restored Pause All would undo the release.
    launch()
    switch phase {
    case .active:
      notifier.withdrawPausedNotice()
      background.sceneDidBecomeActive()
    case .background:
      background.sceneDidEnterBackground()
    case .inactive:
      break
    @unknown default:
      break
    }
  }

  // MARK: Opening NZBs

  /// From `.onOpenURL` and the file importer.
  func open(_ urls: [URL]) async {
    let added = await adding { await router.handle(urls).contains(where: \.isAdded) }
    if added, settings.notifyWhenFinished {
      Task { await notifier.requestAuthorizationIfNeeded() }
    }
  }

  /// Download Again for a duplicate.
  func addAgain(_ duplicate: DuplicateNZB) {
    router.dismissDuplicate()
    Task { await adding { await queue.addAgain(duplicate).isAdded } }
  }

  /// Adds through the cellular gate, which holds the queue meanwhile so
  /// nothing new starts before the user answers, and covers a run that
  /// starts with a background task. True when anything was added.
  @discardableResult
  private func adding(_ add: () async -> Bool) async -> Bool {
    let heldForAdding = gate.prepareToAdd()
    let added = await add()
    gate.finishAdding(addedAny: added, heldForAdding: heldForAdding)
    if added { background.userStartedWork() }
    return added
  }

  // MARK: Actions

  func pause(_ id: DownloadItem.ID) {
    queue.pause(id)
    holds.userPaused(id)
  }

  func resume(_ id: DownloadItem.ID) {
    gate.perform { [queue] in queue.resume(id) }
  }

  /// Start when downloads wait for Start, Retry when failed or stopped.
  func start(_ id: DownloadItem.ID) {
    gate.perform { [queue] in queue.start(id) }
  }

  /// Resume for a waiting download the queue holds back, which its row shows
  /// as paused: Try Again when a server problem is what holds it, otherwise
  /// Start, which lets it run past Pause All.
  func resumeHeld(_ id: DownloadItem.ID) {
    if queue.unresolvedServerProblem != nil {
      retryServer()
    } else {
      start(id)
    }
  }

  func retry(_ id: DownloadItem.ID) {
    gate.perform { [queue] in queue.retry(id) }
  }

  func downloadAnyway(_ id: DownloadItem.ID) {
    gate.perform { [queue] in queue.downloadAnyway(id) }
  }

  /// Post-processing only, so no network: no gate.
  func providePassword(_ id: DownloadItem.ID, password: String) {
    queue.providePassword(id, password: password)
    background.userStartedWork()
  }

  func pauseAll() {
    queue.pauseAll()
    holds.userPausedAll()
  }

  func resumeAll() {
    gate.perform { [queue, holds] in
      holds.userResumed()
      queue.resumeAll()
    }
  }

  func startAll() {
    gate.perform { [queue] in queue.startAll() }
  }

  func stop(_ id: DownloadItem.ID, deletingData: Bool) {
    queue.stop(id, deletingData: deletingData)
  }

  /// Off the list; a running download stops. Its data stays unless
  /// `deletingData`, which deletes what an unfinished download has so far.
  func remove(_ id: DownloadItem.ID, deletingData: Bool = false) {
    if selection == id { selection = nil }
    queue.remove(id, deletingData: deletingData)
  }

  /// Off the list and its folder deleted (iOS has no Trash).
  func deleteFiles(_ id: DownloadItem.ID) {
    if selection == id { selection = nil }
    queue.moveToTrash(id)
  }

  func openServerSettings() {
    dismissServerAlert()
    isShowingSettings = true
  }

  /// Try Again after a server problem: the queue goes back to how the user
  /// had it, through the cellular gate like any other start.
  func retryServer() {
    queue.dismissServerProblem()
    gate.perform { [queue] in queue.retryServer() }
  }

  /// Not Now: the alert goes; the queue stays paused, and the list says why
  /// (`serverProblemNotice`).
  func dismissServerAlert() {
    queue.dismissServerProblem()
  }

  /// Why the queue is paused, for the notice above the list: a server
  /// problem whose alert was put away and that is not resolved yet.
  var serverProblemNotice: EngineError? {
    guard queue.serverProblem == nil else { return nil }
    return queue.unresolvedServerProblem
  }

  /// The server settings are done with (the sheet closed): the engine takes
  /// them now, and the queue tries again if a server problem paused it.
  /// Typing alone never retries, so a half-typed password is not sent.
  func serverSettingsCommitted() {
    model.serverSettingsCommitted()
  }

  // MARK: Events

  private func itemFinished(_ item: DownloadItem) {
    guard settings.notifyWhenFinished, !background.isForeground else { return }
    notifier.post(for: item)
  }

  /// The system ended the app's background time and the queue paused: one
  /// quiet notice, under the same setting as the others.
  private func pausedInBackground() {
    guard settings.notifyWhenFinished else { return }
    notifier.postPausedInBackground()
  }

  #if DEBUG
    /// `-importCLIConfig <path>` takes the server from a dl-nzb CLI config
    /// file through the engine's own reader, the way the Mac's import does, so
    /// testing in the simulator never needs anyone to type or see the password.
    /// `-connectionsOverride <n>` then caps the connections, so a test can share
    /// the provider's limit with another client.
    private func importCLIConfigForTesting() {
      guard let path = UserDefaults.standard.string(forKey: "importCLIConfig"), !path.isEmpty else { return }
      Task {
        do {
          try await model.importCLIConfig(from: URL(filePath: path))
          let cap = UserDefaults.standard.integer(forKey: "connectionsOverride")
          if cap > 0 { settings.connections = cap }
          isShowingOnboarding = !settings.hasServer
          AppLog.open.info("-importCLIConfig: server imported")
        } catch {
          AppLog.open.error("-importCLIConfig failed: \(error.localizedDescription, privacy: .public)")
        }
      }
    }

    /// `-openOnLaunch <path>` adds an NZB, or every NZB in a folder, through
    /// exactly the path `.onOpenURL` takes. For the simulator, which has no
    /// way to send a document to an app from the command line.
    private func openOnLaunchForTesting() {
      guard let path = UserDefaults.standard.string(forKey: "openOnLaunch"), !path.isEmpty else { return }
      let url = URL(filePath: path)
      var urls = [url]
      if url.hasDirectoryPath || (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true {
        let contents = (try? FileManager.default.contentsOfDirectory(at: url, includingPropertiesForKeys: nil)) ?? []
        urls = contents.filter(\.isNZB).sorted { $0.lastPathComponent < $1.lastPathComponent }
      }
      AppLog.open.info("-openOnLaunch: opening \(urls.count) NZBs")
      Task { await open(urls) }
    }
  #endif
}

extension AddResult {
  fileprivate var isAdded: Bool {
    if case .added = self { true } else { false }
  }
}

extension ProcessInfo.ThermalState {
  var logName: String {
    switch self {
    case .nominal: "nominal"
    case .fair: "fair"
    case .serious: "serious"
    case .critical: "critical"
    @unknown default: "unknown"
    }
  }
}
