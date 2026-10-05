import DlNzbKit
import DlNzbUI
import Foundation

/// What the menu bar's commands show and enable.
///
/// SwiftUI rebuilds the whole main menu whenever anything the commands read
/// changes, and a rebuild redraws whichever menu is open: read straight from
/// the queue, an open Edit menu flickered with every progress event. So
/// `MacApp` keeps one of these, replaced only when it differs, and the
/// commands read nothing else.
struct MenuState: Equatable {
  var hasDownloads = false
  var canPauseAll = false
  var canResumeAll = false
  var hasFinished = false
  var inspectorToggleTitle = "Show Inspector"
  /// The selection's commands, while nothing modal is up.
  var selection = ItemCommands.none

  init() {}

  @MainActor
  init(app: MacApp) {
    let queue = app.queue
    hasDownloads = !queue.items.isEmpty
    canPauseAll = queue.canPauseAll
    canResumeAll = queue.canResumeAll
    hasFinished = queue.items.contains(where: \.isFinished)
    inspectorToggleTitle = app.inspectorToggleTitle
    selection = app.isPresentingModal ? .none : ItemCommands(app: app, ids: app.selection)
  }
}

/// Which item commands apply to some downloads, and what Retry is called:
/// the Downloads menu's (from `MenuState`) and the context menu's.
struct ItemCommands: Equatable {
  var ids: Set<DownloadItem.ID> = []
  var canReveal = false
  var canOpen = false
  var canQuickLook = false
  var canPause = false
  var canResume = false
  var canStop = false
  var canRetry = false
  var retryTitle = "Retry"
  var canEnterPassword = false
  var canDownloadAnyway = false

  /// Nothing selected, or the main window is not key.
  static let none = ItemCommands()

  private init() {}

  @MainActor
  init(app: MacApp, ids: Set<DownloadItem.ID>) {
    self.ids = ids
    let chosen = app.items(ids)
    canReveal = !chosen.isEmpty
    // Open and Quick Look take the finished ones.
    canOpen = chosen.contains(where: \.isFinished)
    canQuickLook = canOpen
    canPause = chosen.contains(where: \.canPause)
    // Resume for paused items, Start for ones waiting on it.
    canResume = chosen.contains { $0.canResume || app.queue.awaitsStart($0) }
    canStop = chosen.contains(where: \.canStop)
    let retrying = chosen.filter(\.canRetry)
    canRetry = !retrying.isEmpty
    // "Download Again" when the one item would start over (`StatusText.retryTitle`).
    retryTitle = retrying.count == 1 ? StatusText.retryTitle(for: retrying[0]) : "Retry"
    // Enter Password… and Download Anyway are for one item that asks.
    let single = chosen.count == 1 ? chosen[0].state : nil
    canEnterPassword = single == .needsAttention(.password)
    if case .needsAttention(.unrepairable) = single { canDownloadAnyway = true }
  }
}
