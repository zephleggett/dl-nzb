import AppKit
import DlNzbKit
import DlNzbUI
import QuickLook
import SwiftUI

/// The list: one row per download, in the order they take turns. Waiting
/// rows drag to reorder; Space shows a finished one in Quick Look; ⌫ takes
/// rows off the list; double-click reveals a finished download.
struct DownloadList: View {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue
  @FocusState private var isFocused: Bool

  var body: some View {
    @Bindable var app = app
    List(selection: $app.selection) {
      ForEach(queue.items) { item in
        LiveItem(item, in: queue) { DownloadRow(item: $0) }
          .moveDisabled(!item.isQueued)
      }
      .onMove { queue.move(fromOffsets: $0, toOffset: $1) }
    }
    .listStyle(.inset)
    .background(ScrollerKeyFocusOff())
    // The list, not the toolbar's Add NZB, has the keyboard when the window opens.
    .focused($isFocused)
    .onAppear { isFocused = true }
    .contextMenu(forSelectionType: DownloadItem.ID.self) { ids in
      if !ids.isEmpty {
        DownloadItemMenu(app: app, commands: ItemCommands(app: app, ids: ids))
      }
    } primaryAction: { ids in
      app.primaryAction(ids)
    }
    .onDeleteCommand { app.remove(app.selection) }
    .onCopyCommand {
      app.items(app.selection).map { NSItemProvider(object: $0.title as NSString) }
    }
    .onKeyPress(.space) {
      guard app.previewURL != nil || app.quickLookURL(for: app.selection) != nil else { return .ignored }
      app.toggleQuickLook(app.selection)
      return .handled
    }
    .quickLookPreview($app.previewURL, in: app.quickLookURLs)
  }
}

/// What can be done to some downloads, per the SPEC: Show in Finder, Open,
/// Quick Look, Copy Name, Pause or Resume, Stop, Retry, Enter Password… and
/// Download Anyway for a row that asks, Remove from List, Move to Trash. The
/// list's context menu (which leaves out what does not apply) and the
/// Downloads menu (which keeps every item, disabled, and adds the shortcuts).
struct DownloadItemMenu: View {
  let app: MacApp
  let commands: ItemCommands
  /// The Downloads menu: shortcuts, one name for Copy Name, and Quick Look
  /// that closes again as ⌘Y does.
  var inMenuBar = false

  private var ids: Set<DownloadItem.ID> { commands.ids }

  var body: some View {
    Button("Show in Finder") { app.reveal(ids) }
      .keyboardShortcut(shortcut("r", [.command, .shift]))
      .disabled(!commands.canReveal)
    Button("Open") { app.openFiles(ids) }
      .keyboardShortcut(shortcut(.downArrow))
      .disabled(!commands.canOpen)
    Button("Quick Look") {
      if inMenuBar { app.toggleQuickLook(ids) } else { app.showQuickLook(ids) }
    }
    .keyboardShortcut(shortcut("y"))
    .disabled(!commands.canQuickLook)
    Button(inMenuBar || ids.count == 1 ? "Copy Name" : "Copy Names") { app.copyNames(ids) }
      .disabled(ids.isEmpty)

    Divider()

    if commands.canResume && !commands.canPause {
      Button("Resume") { app.resume(ids) }
    } else {
      Button("Pause") { app.pause(ids) }
        .disabled(!commands.canPause)
    }
    Button("Stop") { app.requestStop(ids) }
      .keyboardShortcut(shortcut("."))
      .disabled(!commands.canStop)
    Button(commands.retryTitle) { app.retry(ids) }
      .keyboardShortcut(shortcut("r"))
      .disabled(!commands.canRetry)
    if inMenuBar || commands.canEnterPassword {
      Button("Enter Password…") {
        if let id = ids.first { app.requestPassword(id) }
      }
      .disabled(!commands.canEnterPassword)
    }
    if inMenuBar || commands.canDownloadAnyway {
      Button("Download Anyway") { app.downloadAnyway(ids) }
        .disabled(!commands.canDownloadAnyway)
    }

    Divider()

    Button("Remove from List") { app.remove(ids) }
      .keyboardShortcut(shortcut(.delete, []))
      .disabled(ids.isEmpty)
    Button("Move to Trash") { app.requestTrash(ids) }
      .keyboardShortcut(shortcut(.delete))
      .disabled(ids.isEmpty)
  }

  private func shortcut(_ key: KeyEquivalent, _ modifiers: EventModifiers = .command) -> KeyboardShortcut? {
    inMenuBar ? KeyboardShortcut(key, modifiers: modifiers) : nil
  }
}

/// With Keyboard Navigation on, Tab would stop on the list's scroll bar on
/// its way out of the list: a stop with nothing to do there, as the arrow
/// keys and Page Up and Down scroll the list already. This takes the list's
/// scrollers out of the key view loop. Placed behind the list, it finds the
/// list's scroll view among its neighbours.
struct ScrollerKeyFocusOff: NSViewRepresentable {
  func makeNSView(context: Context) -> Probe { Probe() }

  func updateNSView(_ view: Probe, context: Context) {
    view.apply()
  }

  final class Probe: NSView {
    private weak var scrollView: NSScrollView?

    override func viewDidMoveToWindow() {
      super.viewDidMoveToWindow()
      // The list's own views are in place once this pass of layout is over.
      Task { @MainActor in self.apply() }
    }

    override func layout() {
      super.layout()
      apply()
    }

    func apply() {
      if scrollView == nil { scrollView = nearestTableScrollView() }
      // Scrollers are replaced when the scroll bar style changes, so this
      // runs again on every layout; setting the flag is cheap.
      for scroller in [scrollView?.verticalScroller, scrollView?.horizontalScroller] {
        scroller?.refusesFirstResponder = true
      }
    }

    private func nearestTableScrollView() -> NSScrollView? {
      var ancestor = superview
      while let view = ancestor {
        if let found = Self.tableScrollView(in: view) { return found }
        ancestor = view.superview
      }
      return nil
    }

    private static func tableScrollView(in view: NSView) -> NSScrollView? {
      if let scroll = view as? NSScrollView, scroll.documentView is NSTableView { return scroll }
      for subview in view.subviews {
        if let found = tableScrollView(in: subview) { return found }
      }
      return nil
    }
  }
}
