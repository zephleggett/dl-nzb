import AppKit
import DlNzbKit
import DlNzbUI
import SwiftUI

/// The menu bar item's menu: what is downloading and how fast, Pause All or
/// Resume All, and the way back to the window and Settings.
struct MenuBarContent: View {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue
  @Environment(\.openSettings) private var openSettings
  @Environment(\.openWindow) private var openWindow

  var body: some View {
    if let item = queue.currentItem {
      Text(MenuBarText.title(item.displayTitle))
      Text(StatusText.line(for: item, in: queue))
      if let others = MenuBarText.others(queue: queue, excluding: item.id) {
        Text(others)
      }
    } else {
      Text(MenuBarText.idle(queue: queue))
    }

    Divider()

    Button(app.toggleAllTitle) { app.toggleAll() }
      .disabled(!app.canToggleAll)

    Divider()

    Button("Open dl-nzb") {
      if app.openWindowAction == nil { app.openWindowAction = openWindow }
      app.showMainWindow()
    }
    Button("Settings…") {
      NSApp.activate()
      openSettings()
    }
    .keyboardShortcut(",")

    Divider()

    Button("Quit dl-nzb") { NSApp.terminate(nil) }
      .keyboardShortcut("q")
  }
}

/// The menu's lines, kept short: a menu grows as wide as its longest item.
@MainActor
enum MenuBarText {
  static let titleLimit = 48

  /// The release name, cut in the middle past `titleLimit` characters.
  static func title(_ title: String) -> String {
    guard title.count > titleLimit else { return title }
    let half = (titleLimit - 1) / 2
    return "\(title.prefix(half))…\(title.suffix(titleLimit - 1 - half))"
  }

  /// The rest of the queue in the window subtitle's words, "1 extracting ·
  /// 3 waiting", or nil when the named item is the only one.
  static func others(queue: DownloadQueue, excluding current: DownloadItem.ID) -> String? {
    let summary = StatusText.queueSummary(for: queue, excluding: current)
    return summary.isEmpty ? nil : summary
  }

  static func idle(queue: DownloadQueue) -> String {
    let summary = StatusText.queueSummary(for: queue)
    return summary.isEmpty ? "No downloads in progress" : summary
  }
}
