import DlNzbKit
import Foundation
import Testing

@testable import DlNzbApp

/// What the menu bar's commands read: worked out from the queue and the
/// selection, and kept by the app so the main menu is rebuilt only when it
/// changes.
@MainActor
@Suite("Menu state")
struct MenuStateTests {
  private func app(_ items: [DownloadItem]) -> MacApp {
    MacApp(model: .preview(items: items), services: nil)
  }

  @Test("The commands follow the queue and the selection")
  func followsSelection() {
    let app = app([PreviewData.downloading, PreviewData.finished, PreviewData.failed])
    let state = MenuState(app: app)
    #expect(state.hasDownloads && state.canPauseAll && !state.canResumeAll && state.hasFinished)
    #expect(state.selection == .none)

    app.selection = [PreviewData.downloading.id]
    let downloading = MenuState(app: app).selection
    #expect(downloading.canPause && downloading.canStop && !downloading.canOpen && !downloading.canRetry)

    app.selection = [PreviewData.finished.id]
    let finished = MenuState(app: app).selection
    #expect(finished.canOpen && finished.canReveal && finished.canQuickLook && !finished.canStop)

    app.selection = [PreviewData.failed.id]
    let failed = MenuState(app: app).selection
    #expect(failed.canRetry && failed.retryTitle == "Download Again")
  }

  @Test("Something modal in the window takes the selection's commands away")
  func modalClearsSelection() {
    let app = app([PreviewData.downloading])
    app.selection = [PreviewData.downloading.id]
    app.isImporting = true
    #expect(MenuState(app: app).selection == .none)
  }

  @Test("The app's menu state catches up with a change on the next turn")
  func keptCurrent() async {
    let app = app([PreviewData.downloading, PreviewData.queued])
    #expect(app.menuState.selection == .none)
    app.selection = [PreviewData.downloading.id]
    for _ in 0..<100 where !app.menuState.selection.canPause {
      try? await Task.sleep(for: .milliseconds(10))
    }
    #expect(app.menuState == MenuState(app: app))
    #expect(app.menuState.selection.canPause)
  }
}
