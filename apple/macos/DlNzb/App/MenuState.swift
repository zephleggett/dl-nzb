import DlNzbKit
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
    canReveal = app.canReveal(ids)
    canOpen = app.canOpenFiles(ids)
    canQuickLook = app.quickLookURL(for: ids) != nil
    canPause = app.canPause(ids)
    canResume = app.canResume(ids)
    canStop = app.canStop(ids)
    canRetry = app.canRetry(ids)
    retryTitle = app.retryTitle(ids)
    canEnterPassword = app.canEnterPassword(ids)
    canDownloadAnyway = app.canDownloadAnyway(ids)
  }
}
