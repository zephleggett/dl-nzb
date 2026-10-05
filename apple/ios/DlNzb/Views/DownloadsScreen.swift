import DlNzbKit
import DlNzbUI
import SwiftUI
import UIKit

/// The list of downloads: the app's one screen on an iPhone, the first column
/// on an iPad. Its toolbar holds Settings on the leading side and [+], the
/// one prominent action, on the trailing side.
struct DownloadsScreen: View {
  @Binding var selection: DownloadItem.ID?
  let settingsTransition: Namespace.ID
  /// Side by side with the detail. Read from the root: inside a split
  /// view's column the size class is compact either way.
  let isSplit: Bool

  @Environment(AppRuntime.self) private var runtime
  /// The rows' questions (stop, remove, delete, password): one for the
  /// list, keyed by the download, so a question outlives a closing swipe and
  /// rows moving around it.
  @State private var prompts = ItemPromptState()

  private var queue: DownloadQueue { runtime.queue }

  var body: some View {
    List(selection: $selection) {
      if !runtime.settings.hasServer && !queue.items.isEmpty {
        NoServerNotice()
      } else if let problem = runtime.serverProblemNotice {
        ServerProblemNotice(problem: problem)
      }
      Section {
        ForEach(queue.items) { item in
          LiveItem(item, in: queue) { DownloadListRow(item: $0, prompts: $prompts) }
        }
      }
      .listSectionSeparator(.hidden, edges: .top)
    }
    .modifier(DownloadsListStyle(isSplit: isSplit))
    .readableWidth()
    .overlay {
      if queue.items.isEmpty {
        ContentUnavailableView {
          Label("No Downloads", systemImage: "arrow.down.circle")
        } description: {
          Text("Open an NZB from Files, Safari or another app.")
        } actions: {
          // Bordered: [+] in the toolbar stays the one prominent action.
          Button("Add NZB…") { runtime.isShowingImporter = true }
            .buttonStyle(.bordered)
        }
      }
    }
    .navigationTitle("Downloads")
    .modifier(QueueSubtitle())
    .toolbar { toolbar }
  }

  @ToolbarContentBuilder private var toolbar: some ToolbarContent {
    ToolbarItem(placement: .topBarLeading) {
      Button {
        runtime.isShowingSettings = true
      } label: {
        Label("Settings", systemImage: "gearshape")
      }
    }
    .matchedTransitionSource(id: SettingsSheet.transitionID, in: settingsTransition)

    if !queue.items.isEmpty {
      ToolbarItem(placement: .topBarTrailing) {
        Menu {
          QueueMenu()
        } label: {
          Label("More", systemImage: "ellipsis")
        }
      }
      ToolbarSpacer(.fixed, placement: .topBarTrailing)
    }

    ToolbarItem(placement: .topBarTrailing) {
      Button {
        runtime.isShowingImporter = true
      } label: {
        Label("Add NZB", systemImage: "plus")
      }
      .buttonStyle(.glassProminent)
    }
  }
}

/// "2 downloading · 84 MB/s", "Paused · 3 waiting", or nothing; and while
/// the cellular question holds the queue, why it is not moving. Its own
/// modifier: the speed changes twice a second, and only the subtitle should
/// redraw with it.
private struct QueueSubtitle: ViewModifier {
  @Environment(AppRuntime.self) private var runtime

  func body(content: Content) -> some View {
    content.navigationSubtitle(subtitle)
  }

  private var subtitle: String {
    let queue = runtime.queue
    if runtime.holds.isHolding(.cellular) {
      return queue.queuedCount > 0 || queue.pausedCount > 0 ? "Waiting for Wi-Fi" : ""
    }
    return StatusText.queueSummary(for: queue)
  }
}

/// Plain rows on a phone, as Mail lists messages; in the split view's
/// first column, the sidebar's own style, whose selection is the inset
/// rounded highlight the system uses there.
private struct DownloadsListStyle: ViewModifier {
  let isSplit: Bool

  func body(content: Content) -> some View {
    if isSplit {
      content.listStyle(.sidebar)
    } else {
      content.listStyle(.plain)
    }
  }
}

/// Pause All or Resume All, Start All, and clearing finished rows.
private struct QueueMenu: View {
  @Environment(AppRuntime.self) private var runtime

  var body: some View {
    let queue = runtime.queue
    if queue.prefersResumeAll {
      Button("Resume All", systemImage: "play.fill") { runtime.resumeAll() }
    } else {
      Button("Pause All", systemImage: "pause.fill") { runtime.pauseAll() }
        .disabled(!queue.canPauseAll)
    }
    if !runtime.settings.startAutomatically && queue.queuedCount > 0 {
      Button("Start All", systemImage: "arrow.down.circle") { runtime.startAll() }
    }
    Divider()
    Button("Remove Finished Downloads", systemImage: "checkmark.circle") { queue.removeAllFinished() }
      .disabled(!queue.items.contains(where: \.isFinished))
  }
}

/// A row with its navigation, swipe actions and context menu.
private struct DownloadListRow: View {
  let item: DownloadItem
  @Binding var prompts: ItemPromptState
  @Environment(AppRuntime.self) private var runtime

  var body: some View {
    NavigationLink(value: item.id) {
      DownloadRowView(item: item, prompts: $prompts)
    }
    .navigationLinkIndicatorVisibility(.hidden)
    .swipeActions(edge: .leading, allowsFullSwipe: true) {
      if runtime.queue.isHeld(item) {
        Button("Resume", systemImage: "play.fill") { runtime.resumeHeld(item.id) }
          .tint(.accentColor)
      } else if runtime.queue.awaitsStart(item) {
        Button("Start", systemImage: "arrow.down") { runtime.start(item.id) }
          .tint(.accentColor)
      } else if item.canResume {
        Button("Resume", systemImage: "play.fill") { runtime.resume(item.id) }
          .tint(.accentColor)
      } else if item.canPause {
        Button("Pause", systemImage: "pause.fill") { runtime.pause(item.id) }
          .tint(.indigo)
      } else if item.canRetry {
        Button(StatusText.retryTitle(for: item), systemImage: "arrow.clockwise") { runtime.retry(item.id) }
          .tint(.accentColor)
      }
    }
    .swipeActions(edge: .trailing, allowsFullSwipe: !item.canStop) {
      if item.canStop {
        Button("Stop", systemImage: "stop.fill") { runtime.requestStop(item, prompts: &prompts) }
          .tint(.orange)
      } else {
        Button("Remove", systemImage: "minus.circle") { runtime.requestRemove(item, prompts: &prompts) }
          .tint(.gray)
      }
      // Red, but not the destructive role: with that role, the list takes
      // the row out at once, before the question is answered.
      Button("Delete", systemImage: "trash") { prompts.ask(.delete, about: item.id) }
        .tint(.red)
    }
    .contextMenu {
      DownloadMenu(item: item, prompts: $prompts)
    }
    // Shown from the row, so the dialog points at the download it is about.
    .itemPrompts(state: $prompts, about: item.id)
  }
}

/// The actions for one download, for its context menu and its detail screen's menu.
struct DownloadMenu: View {
  let item: DownloadItem
  @Binding var prompts: ItemPromptState
  @Environment(AppRuntime.self) private var runtime

  var body: some View {
    Section {
      Button("Copy Name", systemImage: "doc.on.doc") {
        UIPasteboard.general.string = item.title
      }
      FileActionButtons(item: item)
    }
    Section {
      if runtime.queue.isHeld(item) {
        Button("Resume", systemImage: "play") { runtime.resumeHeld(item.id) }
      } else if runtime.queue.awaitsStart(item) {
        Button("Start", systemImage: "arrow.down") { runtime.start(item.id) }
      }
      if item.canResume {
        Button("Resume", systemImage: "play") { runtime.resume(item.id) }
      } else if item.canPause {
        Button("Pause", systemImage: "pause") { runtime.pause(item.id) }
      }
      if item.canRetry {
        Button(StatusText.retryTitle(for: item), systemImage: "arrow.clockwise") { runtime.retry(item.id) }
      }
      if item.canStop {
        Button("Stop…", systemImage: "stop") { runtime.requestStop(item, prompts: &prompts) }
      }
    }
    Section {
      Button(item.needsRemovalConfirmation ? "Remove from List…" : "Remove from List", systemImage: "minus.circle") {
        runtime.requestRemove(item, prompts: &prompts)
      }
      Button("Delete Files…", systemImage: "trash", role: .destructive) { prompts.ask(.delete, about: item.id) }
    }
  }
}

/// Above the list when there is no server yet: the downloads wait for one.
private struct NoServerNotice: View {
  @Environment(AppRuntime.self) private var runtime

  var body: some View {
    VStack(alignment: .leading, spacing: 8) {
      Label("Add your Usenet server to start downloading.", systemImage: "server.rack")
        .font(.subheadline)
      Button("Add Server") { runtime.isShowingSettings = true }
        .buttonStyle(.bordered)
        .controlSize(.small)
    }
    .padding(.vertical, 6)
    .selectionDisabled()
  }
}

/// Above the list once the server problem's alert is put away: the queue is
/// paused, and here is why and what to do, until it runs again.
private struct ServerProblemNotice: View {
  let problem: EngineError
  @Environment(AppRuntime.self) private var runtime
  @Environment(\.dynamicTypeSize) private var dynamicTypeSize

  var body: some View {
    VStack(alignment: .leading, spacing: 8) {
      Label {
        VStack(alignment: .leading, spacing: 2) {
          Text(AlertText.serverProblemTitle(problem.kind))
            .fontWeight(.semibold)
            .fixedSize(horizontal: false, vertical: true)
          Text("Downloads are paused until the server works again.")
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
      } icon: {
        Image(systemName: "exclamationmark.triangle.fill")
          .foregroundStyle(.orange)
          .accessibilityHidden(true)
      }
      .font(.subheadline)
      .accessibilityElement(children: .combine)
      let layout =
        dynamicTypeSize >= .xxLarge ? AnyLayout(VStackLayout(alignment: .leading, spacing: 8)) : AnyLayout(HStackLayout(spacing: 8))
      layout { buttons }
        .buttonStyle(.bordered)
        .controlSize(.small)
    }
    .padding(.vertical, 6)
    .selectionDisabled()
  }

  @ViewBuilder private var buttons: some View {
    Button("Open Settings") { runtime.openServerSettings() }
    Button("Try Again") { runtime.retryServer() }
  }
}
