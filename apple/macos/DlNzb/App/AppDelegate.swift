import AppKit
import DlNzbKit

/// The parts of the app's life SwiftUI does not cover: files opened from
/// Finder and the Dock (`.onOpenURL` never fires for a `Window` scene), the
/// Dock icon's click and menu, and stopping downloads cleanly before quitting.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
  /// Set by `DlNzbApp` once the window (or the menu bar item) exists; opens
  /// that came earlier are handed over then.
  weak var target: (any OpenTarget)? {
    didSet { deliverOpens() }
  }

  /// The app the delegate starts and quits; tests leave it nil.
  var app: MacApp?

  /// How long quitting waits for jobs to stop and the list to be saved.
  var quitTimeout: Duration = .seconds(10)

  /// Opens that arrived before `target` was set.
  private(set) var buffer = OpenRequestBuffer()

  func applicationWillFinishLaunching(_ notification: Notification) {
    // One window, no tabs: the View menu has nothing to offer here.
    NSWindow.allowsAutomaticWindowTabbing = false
    // A test host leaves the app alone: the tests make their own.
    if app == nil && !MacApp.isRunningTests { app = .shared }
    #if DEBUG
      // `-appearance light|dark`: screenshots in either, whatever the Mac is set to.
      switch UserDefaults.standard.string(forKey: "appearance") {
      case "light": NSApp.appearance = NSAppearance(named: .aqua)
      case "dark": NSApp.appearance = NSAppearance(named: .darkAqua)
      default: break
      }
    #endif
  }

  func applicationDidFinishLaunching(_ notification: Notification) {
    app?.launch()
  }

  // MARK: Opening files

  func application(_ application: NSApplication, open urls: [URL]) {
    Log.mac.info("asked to open \(urls.count) files")
    buffer.add(urls)
    deliverOpens()
  }

  private func deliverOpens() {
    guard let target, !buffer.isEmpty else { return }
    let urls = buffer.drain()
    target.showMainWindow()
    target.open(urls)
  }

  // MARK: Windows

  /// Closing the window leaves downloads running; the Dock icon brings it back.
  func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    false
  }

  func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
    if !flag { target?.showMainWindow() }
    return true
  }

  func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
    true
  }

  // MARK: Dock menu

  func applicationDockMenu(_ sender: NSApplication) -> NSMenu? {
    guard let app else { return nil }
    let menu = NSMenu()
    // Otherwise AppKit enables any item whose target answers its action.
    menu.autoenablesItems = false
    let item = NSMenuItem(title: app.toggleAllTitle, action: #selector(toggleAll(_:)), keyEquivalent: "")
    item.target = self
    item.isEnabled = app.canToggleAll
    menu.addItem(item)
    return menu
  }

  @objc private func toggleAll(_ sender: Any?) {
    app?.toggleAll()
  }

  // MARK: Quitting

  /// Asks first when downloads would be interrupted (not when the Mac is
  /// logging out, restarting or shutting down: they continue next time
  /// anyway), then stops every job where it can continue and saves the list
  /// before replying.
  func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
    guard let app else { return .terminateNow }
    if !Self.isSystemQuit, let prompt = QuitPrompt(unfinished: app.queue.unfinishedCount) {
      let alert = NSAlert()
      alert.messageText = prompt.title
      alert.informativeText = prompt.message
      alert.addButton(withTitle: prompt.confirm)
      alert.addButton(withTitle: prompt.cancel)
      guard alert.runModal() == .alertFirstButtonReturn else { return .terminateCancel }
    }
    let timeout = quitTimeout
    Task {
      await withTaskGroup(of: Void.self) { group in
        group.addTask { await app.prepareForQuit() }
        group.addTask { try? await Task.sleep(for: timeout) }
        await group.next()
        group.cancelAll()
      }
      NSApp.reply(toApplicationShouldTerminate: true)
    }
    return .terminateLater
  }

  /// The quit came from logging out, restarting or shutting down.
  private static var isSystemQuit: Bool {
    guard let event = NSAppleEventManager.shared().currentAppleEvent,
      event.eventClass == kCoreEventClass, event.eventID == kAEQuitApplication,
      let reason = event.attributeDescriptor(forKeyword: kAEQuitReason)?.enumCodeValue
    else { return false }
    let systemReasons: [OSType] = [
      OSType(kAELogOut), OSType(kAEReallyLogOut), OSType(kAEShowRestartDialog), OSType(kAERestart), OSType(kAEShowShutdownDialog),
      OSType(kAEShutDown),
    ]
    return systemReasons.contains(reason)
  }
}
