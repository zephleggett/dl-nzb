import AppKit
import DlNzbKit
import DlNzbUI

/// The menu bar item, while Settings has it on: the segmented arrow, filled
/// while something runs, and a menu with what is downloading and how fast,
/// Pause All or Resume All, and the way back to the window and Settings.
///
/// AppKit, as the Dock menu is. SwiftUI rebuilds the whole main menu when a
/// scene's content changes, so a `MenuBarExtra` showing the speed made an
/// open Edit menu flicker with every progress event. This menu is filled as
/// it opens and its lines change in place while it is open; closed, it
/// does nothing.
@MainActor
final class StatusMenu: NSObject, NSMenuDelegate {
  private let app: MacApp
  private let menu = NSMenu()
  private var statusItem: NSStatusItem?
  private var shownActive: Bool?
  /// The lines about the queue at the top of the menu.
  private var lineItems: [NSMenuItem] = []
  private let toggleItem = NSMenuItem(title: "", action: #selector(toggleAll(_:)), keyEquivalent: "")
  /// Keeps the lines current while the menu is open.
  private var watcher: Watcher?

  init(app: MacApp) {
    self.app = app
    super.init()
    menu.delegate = self
    // The lines stay disabled; the commands are enabled by hand.
    menu.autoenablesItems = false
    toggleItem.target = self
  }

  /// Puts the item in the menu bar or takes it out, with the icon for
  /// whether anything runs. Both are template images, so the menu bar
  /// tints them.
  func update(shown: Bool, active: Bool) {
    guard shown else {
      if let statusItem {
        NSStatusBar.system.removeStatusItem(statusItem)
        Log.mac.info("removed the menu bar item")
      }
      statusItem = nil
      shownActive = nil
      return
    }
    let item = statusItem ?? makeStatusItem()
    guard active != shownActive, let button = item.button else { return }
    shownActive = active
    button.image = NSImage(named: active ? "MenuBarIconActive" : "MenuBarIcon")
    button.setAccessibilityLabel(active ? "dl-nzb, downloading" : "dl-nzb")
  }

  private func makeStatusItem() -> NSStatusItem {
    let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    item.menu = menu
    statusItem = item
    Log.mac.info("added the menu bar item")
    return item
  }

  // MARK: The menu

  func menuNeedsUpdate(_ menu: NSMenu) {
    menu.removeAllItems()
    lineItems = MenuBarText.lines(for: app.queue).map(Self.lineItem)
    for item in lineItems { menu.addItem(item) }
    menu.addItem(.separator())
    updateToggle()
    menu.addItem(toggleItem)
    menu.addItem(.separator())
    menu.addItem(commandItem("Open dl-nzb", #selector(openWindow(_:))))
    menu.addItem(commandItem("Settings…", #selector(openSettings(_:)), key: ","))
    menu.addItem(.separator())
    menu.addItem(commandItem("Quit dl-nzb", #selector(quit(_:)), key: "q"))
  }

  func menuWillOpen(_ menu: NSMenu) {
    watcher = Watcher { [weak self] in self?.refresh() }
  }

  func menuDidClose(_ menu: NSMenu) {
    watcher?.stop()
    watcher = nil
  }

  /// The lines and Pause All, as the queue moves on under the open menu.
  private func refresh() {
    let lines = MenuBarText.lines(for: app.queue)
    if lines.count == lineItems.count {
      for (item, line) in zip(lineItems, lines) where item.title != line {
        item.title = line
      }
    } else {
      // A download started or finished: a different set of lines.
      for item in lineItems { menu.removeItem(item) }
      lineItems = lines.map(Self.lineItem)
      for (index, item) in lineItems.enumerated() { menu.insertItem(item, at: index) }
    }
    updateToggle()
  }

  private func updateToggle() {
    toggleItem.title = app.toggleAllTitle
    toggleItem.isEnabled = app.canToggleAll
  }

  private static func lineItem(_ line: String) -> NSMenuItem {
    let item = NSMenuItem(title: line, action: nil, keyEquivalent: "")
    item.isEnabled = false
    return item
  }

  private func commandItem(_ title: String, _ action: Selector, key: String = "") -> NSMenuItem {
    let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
    item.target = self
    return item
  }

  @objc private func toggleAll(_ sender: Any?) {
    app.toggleAll()
  }

  @objc private func openWindow(_ sender: Any?) {
    app.showMainWindow()
  }

  @objc private func openSettings(_ sender: Any?) {
    app.showSettings()
  }

  @objc private func quit(_ sender: Any?) {
    NSApp.terminate(nil)
  }
}

/// The menu's lines, kept short: a menu grows as wide as its longest item.
@MainActor
enum MenuBarText {
  static let titleLimit = 48

  /// What is downloading (or else being processed) and how it stands, and
  /// the rest of the queue; or the queue in a line when nothing runs. The
  /// reader updates with the progress.
  static func lines(for queue: DownloadQueue) -> [String] {
    guard let current = queue.currentItem else { return [idle(queue: queue)] }
    let item = queue.live(current)
    var lines = [title(item.displayTitle), StatusText.line(for: item, in: queue)]
    if let rest = others(queue: queue, excluding: item.id) { lines.append(rest) }
    return lines
  }

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
