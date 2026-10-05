import AppKit
import DlNzbKit
import DlNzbUI
import SwiftUI

/// The one window: the list of downloads with a trailing inspector, the
/// toolbar, the drop target, and every alert and sheet the queue asks for.
///
/// Its body reads none of the items, so a change in the list redraws the
/// list, the subtitle and the toolbar, not the whole window with its
/// inspector, sheets and alerts.
struct MainWindow: View {
  @Environment(MacApp.self) private var app
  @Environment(\.openWindow) private var openWindow
  @Environment(\.openSettings) private var openSettings
  @State private var isDropTargeted = false

  var body: some View {
    @Bindable var app = app
    DownloadsContent()
      .safeAreaInset(edge: .top, spacing: 0) { ServerProblemNotice() }
      .frame(minWidth: 360, minHeight: 300)
      .overlay { DropHighlight(isActive: isDropTargeted) }
      .dropDestination(for: URL.self) { urls, _ in
        let nzbs = urls.filter(\.isNZB)
        guard !nzbs.isEmpty else { return false }
        app.open(nzbs)
        return true
      } isTargeted: {
        // Only NZBs light the window up; anything else would be turned away.
        isDropTargeted = $0 && DragPasteboard.holdsNZB()
      }
      .navigationTitle("dl-nzb")
      .modifier(QueueSubtitle())
      .toolbar { MainToolbar(app: app) }
      .modifier(DownloadInspector())
      .focusedSceneValue(\.downloadsWindow, true)
      .fileImporter(isPresented: $app.isImporting, allowedContentTypes: [.nzb], allowsMultipleSelection: true) { result in
        switch result {
        case .success(let urls): app.open(urls)
        case .failure(let error): Log.mac.error("the open panel failed: \(error.localizedDescription, privacy: .public)")
        }
      }
      .modifier(QueueAlerts())
      .sheet(isPresented: $app.isOnboarding) { OnboardingSheet() }
      .sheet(item: $app.passwordRequest) { PasswordSheet(request: $0) }
      .onAppear {
        app.openWindowAction = openWindow
        app.openSettingsAction = openSettings
      }
  }
}

/// The trailing inspector, shown as the user left it, except while the list
/// is empty: there is nothing to inspect, and a "No Selection" column would
/// take a third of a new window.
struct DownloadInspector: ViewModifier {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue

  func body(content: Content) -> some View {
    let isEmpty = queue.items.isEmpty
    content
      .inspector(isPresented: Binding(get: { app.showsInspector && !isEmpty }, set: { if !isEmpty { app.showsInspector = $0 } })) {
        InspectorPanel()
          .inspectorColumnWidth(min: 240, ideal: 280, max: 420)
      }
  }
}

/// What is being dragged over the window, read from the drag pasteboard
/// while it hovers (the drop itself goes through `dropDestination`).
enum DragPasteboard {
  /// Whether the drag carries at least one NZB file.
  static func holdsNZB() -> Bool {
    let objects = NSPasteboard(name: .drag).readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true])
    return (objects as? [URL] ?? []).contains(where: \.isNZB)
  }
}

/// While a server problem holds the queue: what went wrong, under the
/// toolbar, with Open Settings and Try Again. The alert says it once; this
/// stays until the problem is resolved, so a dismissed alert does not leave
/// the list paused with no reason given.
struct ServerProblemNotice: View {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue
  @Environment(\.openSettings) private var openSettings

  var body: some View {
    if let problem = queue.unresolvedServerProblem {
      HStack(alignment: .firstTextBaseline, spacing: 10) {
        Image(systemName: "exclamationmark.triangle.fill")
          .foregroundStyle(StatusTone.warning.glyphStyle)
          .accessibilityHidden(true)
        VStack(alignment: .leading, spacing: 6) {
          VStack(alignment: .leading, spacing: 2) {
            Text(AlertText.serverProblemTitle(problem.kind))
              .font(.headline)
            Text(AlertText.serverProblemMessage(problem))
              .font(.subheadline)
              .fixedSize(horizontal: false, vertical: true)
          }
          HStack(spacing: 8) {
            Button("Open Settings") {
              app.prepareServerSettings()
              openSettings()
            }
            Button("Try Again") { queue.retryServer() }
          }
          .controlSize(.small)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
      }
      .padding(.horizontal, 16)
      .padding(.vertical, 10)
      .background(.background.secondary)
      .overlay(alignment: .bottom) { Divider() }
      .accessibilityElement(children: .contain)
    }
  }
}

/// The list, or what to do while it is empty.
struct DownloadsContent: View {
  @Environment(DownloadQueue.self) private var queue

  var body: some View {
    if queue.items.isEmpty {
      EmptyDownloads()
    } else {
      DownloadList()
    }
  }
}

/// The window's subtitle: "2 downloading · 84 MB/s", "Paused · 3 waiting",
/// or a missing server.
struct QueueSubtitle: ViewModifier {
  @Environment(DownloadQueue.self) private var queue
  @Environment(SettingsStore.self) private var settings

  func body(content: Content) -> some View {
    content.navigationSubtitle(subtitle)
  }

  private var subtitle: String {
    if !settings.hasServer && !queue.items.isEmpty { return "Waiting for a server" }
    return StatusText.queueSummary(for: queue)
  }
}

/// Pause All or Resume All, Add NZB (the one prominent action), and the
/// inspector toggle. The system draws the glass; nothing here adds a
/// background or tints a glyph.
struct MainToolbar: ToolbarContent {
  let app: MacApp

  var body: some ToolbarContent {
    ToolbarItem(placement: .primaryAction) {
      Button {
        app.toggleAll()
      } label: {
        Label(app.toggleAllTitle, systemImage: app.queue.prefersResumeAll ? "play.fill" : "pause.fill")
      }
      .help(app.toggleAllTitle)
      .disabled(!app.canToggleAll)
    }
    ToolbarItem(placement: .primaryAction) {
      Button {
        app.presentOpenPanel()
      } label: {
        Label("Add NZB", systemImage: "plus")
      }
      .buttonStyle(.borderedProminent)
      .help("Add NZB files")
    }
    ToolbarSpacer(.fixed, placement: .primaryAction)
    ToolbarItem(placement: .primaryAction) {
      Button {
        app.showsInspector.toggle()
      } label: {
        Label(app.inspectorToggleTitle, systemImage: "sidebar.trailing")
      }
      .help(app.inspectorToggleTitle)
      .disabled(app.queue.items.isEmpty)
    }
  }
}

/// No downloads yet: what to do about it.
struct EmptyDownloads: View {
  @Environment(MacApp.self) private var app

  var body: some View {
    ContentUnavailableView {
      Label("No Downloads", systemImage: "arrow.down.circle")
    } description: {
      Text("Add an NZB file, or drop one here.")
    } actions: {
      Button("Add NZB…") { app.presentOpenPanel() }
    }
  }
}

/// The whole window lights up while an NZB hovers over it.
struct DropHighlight: View {
  let isActive: Bool

  var body: some View {
    RoundedRectangle(cornerRadius: 12, style: .continuous)
      .strokeBorder(Color.accentColor, lineWidth: 3)
      .background(Color.accentColor.opacity(0.08), in: .rect(cornerRadius: 12, style: .continuous))
      .padding(6)
      .opacity(isActive ? 1 : 0)
      .animation(.easeOut(duration: 0.15), value: isActive)
      .allowsHitTesting(false)
      .accessibilityHidden(true)
  }
}
