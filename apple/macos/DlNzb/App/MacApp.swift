import AppKit
import DlNzbKit
import DlNzbRust
import DlNzbUI
import Observation
import SwiftUI

/// The Mac app's composition root: the shared `AppModel`, the system
/// services that mirror the queue (Dock tile, Finder progress, sleep,
/// notifications, Sparkle), and the bits of window state that the window,
/// the menus, the Dock menu and the menu bar item all act on.
///
/// One exists for the life of the app (`shared`), so the app delegate, the
/// scenes and App Intents reach the same queue.
@MainActor
@Observable
final class MacApp: OpenTarget {
  static let shared = MacApp()

  let model: AppModel
  var settings: SettingsStore { model.settings }
  var queue: DownloadQueue { model.queue }

  // MARK: Window state

  /// The rows selected in the main window; menu commands act on these.
  var selection: Set<DownloadItem.ID> = []

  var showsInspector: Bool {
    didSet { UserDefaults.standard.set(showsInspector, forKey: Self.showsInspectorKey) }
  }

  /// Opened again: "Already in the List" or "Already Downloaded", one at a time.
  var duplicates: [DuplicateNZB] = []
  /// Files that could not be added, for one alert.
  var addProblems: [AddProblem] = []
  /// The open panel (⌘O, Add NZB).
  var isImporting = false
  /// First launch without a server, or Settings asked for again.
  var isOnboarding = false
  /// Stop on items that have data: keep it or delete it.
  var stopRequest: ItemRequest?
  /// Move to Trash asks first.
  var trashRequest: ItemRequest?
  /// Remove from List on items that are running or have data: keep the data
  /// or delete it.
  var removeRequest: ItemRequest?
  /// Enter Password… on a Password Required row.
  var passwordRequest: ItemRequest?
  /// The pane the Settings window shows; Open Settings from a server problem
  /// goes to Server.
  var settingsPane: SettingsPane = .general
  /// The file Quick Look is showing.
  var previewURL: URL?
  /// What the menu bar's commands show, replaced only when it changes.
  private(set) var menuState = MenuState()

  /// Something modal is up in the main window, so the Downloads menu's
  /// commands (⌫, ⌘⌫ among them) leave the keyboard to it.
  var isPresentingModal: Bool {
    !duplicates.isEmpty || !addProblems.isEmpty || isImporting || isOnboarding || stopRequest != nil || trashRequest != nil
      || removeRequest != nil || passwordRequest != nil || queue.serverProblem != nil || queue.actionError != nil
  }

  // MARK: Services

  @ObservationIgnored let services: SystemServices?
  #if DIRECT
    let updates: UpdateController
  #endif
  /// The main window's openWindow, kept so Finder opens, the Dock and the
  /// menu bar item can bring the window back after it was closed.
  @ObservationIgnored var openWindowAction: OpenWindowAction?
  /// The main window's openSettings, for the menu bar item's Settings…:
  /// AppKit's own way no longer opens a SwiftUI Settings scene.
  @ObservationIgnored var openSettingsAction: OpenSettingsAction?
  @ObservationIgnored private var menuStateWatcher: Watcher?
  @ObservationIgnored private var isLaunched = false

  private static let showsInspectorKey = "showsInspector"

  /// - Parameters:
  ///   - model: The app's model; by default the real one (or, in a test
  ///     host, one that cannot touch the user's queue or settings).
  ///   - services: Dock, Finder, sleep and notifications; off in tests.
  init(model: AppModel? = nil, services: SystemServices? = nil) {
    let testing = Self.isRunningTests
    self.model = model ?? (testing ? Self.makeTestHostModel() : Self.makeModel())
    if model == nil && !testing {
      // The list is read before any window is built, so the window's first
      // frame already has it. Restored later, at launch, the window sometimes
      // missed the change and showed No Downloads until something else
      // changed. `launch()` restoring again is a no-op.
      self.model.queue.restore()
    }
    self.services = services ?? (testing ? nil : SystemServices())
    showsInspector = UserDefaults.standard.object(forKey: Self.showsInspectorKey) as? Bool ?? true
    #if DIRECT
      updates = UpdateController(enabled: !testing)
    #endif
    menuStateWatcher = Watcher { [weak self] in self?.updateMenuState() }
  }

  private func updateMenuState() {
    let state = MenuState(app: self)
    if state != menuState { menuState = state }
  }

  /// Builds the model with the engine for this launch.
  ///
  /// `-simulate YES` asks for the simulated engine; otherwise the Rust one.
  private static func makeModel() -> AppModel {
    let makeEngine: (AppModel.EngineKind) -> any DownloadEngine = { kind in
      switch kind {
      case .rust: RustEngine()
      case .simulated: SimulatedEngine()
      }
    }
    #if DEBUG
      // `-scratchState YES`: settings, password and queue of this run only, so
      // trying the app (screenshots, demos) leaves the real ones alone.
      if UserDefaults.standard.bool(forKey: "scratchState") {
        // `-scratchStateName <name>` keeps that scratch queue and its settings
        // across launches (a demo can quit and pick up where it was); without
        // it, every launch starts empty.
        let name = UserDefaults.standard.string(forKey: "scratchStateName").flatMap { $0.isEmpty ? nil : $0 }
        let suite = "com.zephleggett.dl-nzb.scratch" + (name.map { ".\($0)" } ?? "")
        if name == nil { UserDefaults.standard.removePersistentDomain(forName: suite) }
        let defaults = UserDefaults(suiteName: suite) ?? .standard
        let settings = SettingsStore(defaults: defaults, passwords: InMemoryPasswordStore())
        let storage =
          name.map {
            QueueStorage(directory: FileManager.default.temporaryDirectory.appending(path: "dl-nzb-scratch-\($0)", directoryHint: .isDirectory))
          } ?? .temporary()
        return AppModel(settings: settings, storage: storage, makeEngine: makeEngine)
      }
    #endif
    return AppModel(makeEngine: makeEngine)
  }

  /// The test host's model: no server, temporary storage, settings in a
  /// scratch suite, so running the tests never touches the user's queue.
  private static func makeTestHostModel() -> AppModel {
    AppModel(settings: .preview(host: ""), storage: .temporary(), simulate: true) { _ in SimulatedEngine(configuration: .fast()) }
  }

  /// XCTest (which hosts Swift Testing) loaded this process.
  nonisolated static var isRunningTests: Bool {
    ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil || NSClassFromString("XCTestCase") != nil
  }

  // MARK: Life

  /// Called once the app has finished launching. Restores the queue, starts
  /// the system services and asks for a server if there is none.
  func launch() {
    guard !isLaunched else { return }
    isLaunched = true
    model.launch()
    services?.start(app: self)
    if !settings.hasServer && !Self.isRunningTests {
      isOnboarding = true
    }
  }

  func prepareForQuit() async {
    await model.prepareForQuit()
    services?.stop()
  }

  // MARK: OpenTarget

  func open(_ urls: [URL]) {
    Task { await openNow(urls) }
  }

  /// Adds NZBs and turns what happened into the window's alerts. The last
  /// one added is selected, so the inspector shows it.
  func openNow(_ urls: [URL]) async {
    guard !urls.isEmpty else { return }
    let results = await model.open(urls)
    var added: DownloadItem.ID?
    for result in results {
      switch result {
      case .added(let id):
        added = id
      case .duplicate(let duplicate):
        duplicates.append(duplicate)
      case .failed(let fileName, let message):
        addProblems.append(AddProblem(fileName: fileName, message: message))
      }
    }
    if let added { selection = [added] }
  }

  static func isMainWindow(_ window: NSWindow) -> Bool {
    window.identifier?.rawValue == "main"
  }

  func showMainWindow() {
    NSApp.activate()
    // A minimised window comes back as it was; openWindow is only for one
    // that was closed.
    if let window = NSApp.windows.first(where: { Self.isMainWindow($0) }), window.isVisible || window.isMiniaturized {
      if window.isMiniaturized { window.deminiaturize(nil) }
      window.makeKeyAndOrderFront(nil)
    } else {
      openWindowAction?(id: "main")
    }
  }

  /// ⌘O, Add NZB and the empty list's Add NZB…: the window's file importer.
  func presentOpenPanel() {
    showMainWindow()
    isImporting = true
  }

  /// Download Again for the duplicate at the front.
  func downloadAgain(_ duplicate: DuplicateNZB) {
    duplicates.removeAll { $0.id == duplicate.id }
    Task {
      let result = await queue.addAgain(duplicate)
      switch result {
      case .added(let id): selection = [id]
      case .failed(let fileName, let message): addProblems.append(AddProblem(fileName: fileName, message: message))
      case .duplicate: break
      }
    }
  }

  /// The duplicate alert's Show in List or Show in Finder, or Cancel: an
  /// earlier copy still to finish is selected in the list (its folder may
  /// not exist yet); a downloaded one's folder is shown in Finder.
  func dismissDuplicate(_ duplicate: DuplicateNZB, revealing: Bool) {
    duplicates.removeAll { $0.id == duplicate.id }
    guard revealing else { return }
    if let listed = queue.listedItem(for: duplicate) {
      selection = [listed.id]
      showMainWindow()
    } else {
      FileActions.reveal([duplicate.folder])
    }
  }

  // MARK: Queue-wide actions

  /// Pause All, or Resume All once nothing runs: the toolbar, the menu bar
  /// item and the Dock menu.
  var toggleAllTitle: String { queue.prefersResumeAll ? "Resume All" : "Pause All" }

  /// Whether that command would do anything: off once everything has
  /// finished, rather than a Pause All with nothing to pause.
  var canToggleAll: Bool { queue.prefersResumeAll ? queue.canResumeAll : queue.canPauseAll }

  func toggleAll() {
    if queue.prefersResumeAll { queue.resumeAll() } else { queue.pauseAll() }
  }

  /// The toolbar button's and the View menu's name for ⌘I.
  var inspectorToggleTitle: String { showsInspector && !queue.items.isEmpty ? "Hide Inspector" : "Show Inspector" }

  // MARK: Item actions (the selection, a context menu's rows, a row's buttons)

  /// The items for these ids, in list order.
  func items(_ ids: Set<DownloadItem.ID>) -> [DownloadItem] {
    queue.items.filter { ids.contains($0.id) }
  }

  func canReveal(_ ids: Set<DownloadItem.ID>) -> Bool { !ids.isEmpty && !items(ids).isEmpty }

  func reveal(_ ids: Set<DownloadItem.ID>) {
    FileActions.reveal(items(ids).map(FileActions.revealTarget))
  }

  func canOpenFiles(_ ids: Set<DownloadItem.ID>) -> Bool { items(ids).contains(where: \.isFinished) }

  func openFiles(_ ids: Set<DownloadItem.ID>) {
    for item in items(ids) where item.isFinished {
      FileActions.open(FileActions.openTarget(for: item))
    }
  }

  func copyNames(_ ids: Set<DownloadItem.ID>) {
    let names = items(ids).map(\.title)
    guard !names.isEmpty else { return }
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(names.joined(separator: "\n"), forType: .string)
  }

  func canPause(_ ids: Set<DownloadItem.ID>) -> Bool { items(ids).contains(where: \.canPause) }

  func pause(_ ids: Set<DownloadItem.ID>) {
    for item in items(ids) where item.canPause { queue.pause(item.id) }
  }

  /// Resume for paused items, Start for ones waiting on it.
  func canResume(_ ids: Set<DownloadItem.ID>) -> Bool {
    items(ids).contains { $0.canResume || queue.awaitsStart($0) }
  }

  func resume(_ ids: Set<DownloadItem.ID>) {
    for item in items(ids) {
      if item.canResume {
        queue.resume(item.id)
      } else if queue.awaitsStart(item) {
        queue.start(item.id)
      }
    }
  }

  func canRetry(_ ids: Set<DownloadItem.ID>) -> Bool { items(ids).contains(where: \.canRetry) }

  func retry(_ ids: Set<DownloadItem.ID>) {
    for item in items(ids) where item.canRetry { queue.retry(item.id) }
  }

  /// "Retry", or "Download Again" when the one item selected would start
  /// over (`StatusText.retryTitle`).
  func retryTitle(_ ids: Set<DownloadItem.ID>) -> String {
    let chosen = items(ids).filter(\.canRetry)
    return chosen.count == 1 ? StatusText.retryTitle(for: chosen[0]) : "Retry"
  }

  /// One item selected that a pre-flight scan stopped: Download Anyway.
  func canDownloadAnyway(_ ids: Set<DownloadItem.ID>) -> Bool {
    let chosen = items(ids)
    guard chosen.count == 1, case .needsAttention(.unrepairable) = chosen[0].state else { return false }
    return true
  }

  func downloadAnyway(_ ids: Set<DownloadItem.ID>) {
    guard canDownloadAnyway(ids), let id = ids.first else { return }
    queue.downloadAnyway(id)
  }

  /// One item selected that waits for its archive's password: Enter Password….
  func canEnterPassword(_ ids: Set<DownloadItem.ID>) -> Bool {
    let chosen = items(ids)
    guard chosen.count == 1, case .needsAttention(.password) = chosen[0].state else { return false }
    return true
  }

  func canStop(_ ids: Set<DownloadItem.ID>) -> Bool { items(ids).contains(where: \.canStop) }

  /// Stops at once when nothing has downloaded yet; otherwise asks whether
  /// to keep the data (so Retry continues) or delete it.
  func requestStop(_ ids: Set<DownloadItem.ID>) {
    let stopping = items(ids).filter(\.canStop)
    guard !stopping.isEmpty else { return }
    if stopping.contains(where: \.hasData) {
      stopRequest = ItemRequest(stopping)
    } else {
      for item in stopping { queue.stop(item.id) }
    }
  }

  func stop(_ request: ItemRequest, deletingData: Bool) {
    stopRequest = nil
    for id in request.ids { queue.stop(id, deletingData: deletingData) }
  }

  /// Remove from List. Asks first when an item is running (it would stop)
  /// or has data that could be deleted; otherwise removes at once.
  func remove(_ ids: Set<DownloadItem.ID>) {
    let removing = items(ids)
    guard !removing.isEmpty else { return }
    if removing.contains(where: \.needsRemovalConfirmation) {
      removeRequest = ItemRequest(removing)
    } else {
      remove(ItemRequest(removing), deletingData: false)
    }
  }

  /// Removes the items, deleting what the unfinished ones downloaded if asked.
  /// A finished download's files always stay.
  func remove(_ request: ItemRequest, deletingData: Bool) {
    removeRequest = nil
    let removed = Set(request.ids)
    let next = selectionAfterRemoving(removed)
    for id in request.ids { queue.remove(id, deletingData: deletingData) }
    if !selection.isDisjoint(with: removed) { selection = next }
  }

  func requestTrash(_ ids: Set<DownloadItem.ID>) {
    let trashing = items(ids)
    guard !trashing.isEmpty else { return }
    trashRequest = ItemRequest(trashing)
  }

  func moveToTrash(_ request: ItemRequest) {
    trashRequest = nil
    let removed = Set(request.ids)
    let next = selectionAfterRemoving(removed)
    for id in request.ids { queue.moveToTrash(id) }
    if !selection.isDisjoint(with: removed) { selection = next }
  }

  /// As in Finder and Mail: the row after the last one removed, or the one
  /// before when the removed rows ran to the end, so ⌫ can be pressed again.
  func selectionAfterRemoving(_ ids: Set<DownloadItem.ID>) -> Set<DownloadItem.ID> {
    let list = queue.items
    guard let last = list.lastIndex(where: { ids.contains($0.id) }) else { return selection.subtracting(ids) }
    if let after = list[(last + 1)...].first(where: { !ids.contains($0.id) }) { return [after.id] }
    if let before = list[..<last].last(where: { !ids.contains($0.id) }) { return [before.id] }
    return []
  }

  func requestPassword(_ id: DownloadItem.ID) {
    guard let item = queue.item(id), case .needsAttention(.password) = item.state else { return }
    passwordRequest = ItemRequest([item])
  }

  func providePassword(_ password: String, for request: ItemRequest) {
    passwordRequest = nil
    guard let id = request.ids.first else { return }
    queue.providePassword(id, password: password)
  }

  /// Double-click: finished downloads are revealed, a locked one asks for its
  /// password, anything else shows the inspector.
  func primaryAction(_ ids: Set<DownloadItem.ID>) {
    let chosen = items(ids)
    if chosen.contains(where: \.isFinished) {
      reveal(Set(chosen.filter(\.isFinished).map(\.id)))
    } else if chosen.count == 1, let item = chosen.first, case .needsAttention(.password) = item.state {
      requestPassword(item.id)
    } else {
      showsInspector = true
    }
  }

  /// What Quick Look shows for these items: the first finished one's.
  func quickLookURL(for ids: Set<DownloadItem.ID>) -> URL? {
    items(ids).first(where: \.isFinished).map(FileActions.previewTarget)
  }

  /// Every finished download's file, for Quick Look's arrows.
  var quickLookURLs: [URL] {
    queue.items.filter(\.isFinished).map(FileActions.previewTarget)
  }

  /// Quick Look in the context menu: show these items, whatever is showing.
  func showQuickLook(_ ids: Set<DownloadItem.ID>) {
    previewURL = quickLookURL(for: ids)
  }

  /// Space and ⌘Y: show the selection in Quick Look, or close it.
  func toggleQuickLook(_ ids: Set<DownloadItem.ID>) {
    previewURL = previewURL == nil ? quickLookURL(for: ids) : nil
  }

  // MARK: Settings

  /// Open Settings from a server problem: the Server pane.
  func prepareServerSettings() {
    settingsPane = .server
    queue.dismissServerProblem()
  }

  /// Try Again after a server problem.
  func retryServer() {
    queue.retryServer()
  }

  /// Settings… in the menu bar item's menu.
  func showSettings() {
    NSApp.activate()
    openSettingsAction?()
  }

  func openAcknowledgements() {
    NSApp.activate()
    openWindowAction?(id: "acknowledgements")
  }
}

/// A file that could not be added, and why.
struct AddProblem: Identifiable, Equatable {
  let id = UUID()
  let fileName: String
  let message: String
}

/// Items a confirmation or sheet is about, with the name it shows.
struct ItemRequest: Identifiable, Equatable {
  let id = UUID()
  let ids: [DownloadItem.ID]
  /// The release name for one item (with its copy number), "3 downloads" for several.
  let name: String
  /// Some of the items are running, for the remove confirmation's message.
  let hasRunning: Bool
  /// Some of the items are finished downloads, whose files always stay.
  let hasFinished: Bool

  init(_ items: [DownloadItem]) {
    ids = items.map(\.id)
    name = items.count == 1 ? items[0].displayTitle : Format.count(items.count, "download", "downloads")
    hasRunning = items.contains(where: \.isRunning)
    hasFinished = items.contains(where: \.isFinished)
  }

  var isSingle: Bool { ids.count == 1 }
}

enum SettingsPane: String, Hashable {
  case general
  case server
  case processing
  case advanced
}
