import DlNzbKit
import DlNzbUI
import SwiftUI

/// One download: the shared row content with the Mac's inline buttons at the
/// trailing edge, and, for a row that needs the user, its actions under the
/// status line.
struct DownloadRow: View {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue
  let item: DownloadItem
  @State private var isHovering = false

  var body: some View {
    HStack(spacing: 10) {
      VStack(alignment: .leading, spacing: 6) {
        DownloadRowContent(item, held: queue.isHeld(item))
          .frame(maxWidth: .infinity, alignment: .leading)
        if case .needsAttention(let attention) = item.state {
          AttentionActions(item: item, attention: attention)
            // Under the title, past the 28-point icon and its spacing.
            .padding(.leading, 38)
        }
      }
      RowButtons(item: item)
        // A waiting row keeps its controls out of sight until it is pointed
        // at or selected, so a long queue reads calmly.
        .opacity(item.isQueued && !isHovering && !app.selection.contains(item.id) ? 0 : 1)
    }
    .padding(.vertical, 3)
    .onHover { isHovering = $0 }
    .modifier(RowAccessibilityActions(item: item))
  }
}

/// The row's controls as VoiceOver actions too, so a waiting row's hidden
/// buttons are reachable without the pointer.
struct RowAccessibilityActions: ViewModifier {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue
  let item: DownloadItem

  func body(content: Content) -> some View {
    content
      .accessibilityActions {
        if queue.awaitsStart(item) {
          Button("Start") { queue.start(item.id) }
        } else if item.canPause {
          Button("Pause") { queue.pause(item.id) }
        }
        if item.canResume {
          Button("Resume") { queue.resume(item.id) }
        }
        if item.canStop {
          Button("Stop") { app.requestStop([item.id]) }
        }
        if item.canRetry {
          Button(StatusText.retryTitle(for: item)) { queue.retry(item.id) }
        }
        switch item.state {
        case .needsAttention(.password):
          Button("Enter Password…") { app.requestPassword(item.id) }
        case .needsAttention(.unrepairable):
          Button("Download Anyway") { queue.downloadAnyway(item.id) }
        default:
          EmptyView()
        }
        Button("Show in Finder") { app.reveal([item.id]) }
        Button("Remove from List") { app.remove([item.id]) }
      }
  }
}

/// Pause or resume and stop while active, a magnifier once finished, Retry
/// (or Download Again, when it would start over) when it failed or was
/// stopped.
struct RowButtons: View {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue
  let item: DownloadItem

  var body: some View {
    HStack(spacing: 2) {
      if queue.awaitsStart(item) {
        RowButton("Start", systemImage: "play.circle.fill") { queue.start(item.id) }
      } else if item.canPause {
        RowButton("Pause", systemImage: "pause.circle.fill") { queue.pause(item.id) }
      }
      if item.canResume {
        RowButton("Resume", systemImage: "play.circle.fill") { queue.resume(item.id) }
      }
      if item.canStop {
        RowButton("Stop", systemImage: "xmark.circle.fill") { app.requestStop([item.id]) }
      }
      if item.isFinished {
        RowButton("Show in Finder", systemImage: "magnifyingglass.circle.fill") { app.reveal([item.id]) }
      }
      if item.canRetry && !item.needsAttention {
        RowButton(StatusText.retryTitle(for: item), systemImage: "arrow.clockwise.circle.fill") { queue.retry(item.id) }
      }
    }
  }
}

/// A borderless circle glyph with its name as tooltip and VoiceOver label.
struct RowButton: View {
  let title: String
  let systemImage: String
  let action: () -> Void

  init(_ title: String, systemImage: String, action: @escaping () -> Void) {
    self.title = title
    self.systemImage = systemImage
    self.action = action
  }

  var body: some View {
    Button(action: action) {
      // Safari's downloads: a grey disc with the glyph cut out of it.
      Label(title, systemImage: systemImage)
        .labelStyle(.iconOnly)
        .symbolRenderingMode(.monochrome)
        .font(.title2)
        .foregroundStyle(.secondary)
        .contentShape(.circle)
    }
    .buttonStyle(.borderless)
    .help(title)
  }
}

/// Download Anyway and Remove, Enter Password…, or Retry and Remove.
struct AttentionActions: View {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue
  let item: DownloadItem
  let attention: DownloadItem.Attention

  var body: some View {
    HStack(spacing: 8) {
      switch attention {
      case .unrepairable:
        Button("Download Anyway") { queue.downloadAnyway(item.id) }
        Button("Remove") { app.remove([item.id]) }
      case .password:
        Button("Enter Password…") { app.requestPassword(item.id) }
      case .diskFull:
        Button(StatusText.retryTitle(for: item)) { queue.retry(item.id) }
        Button("Remove") { app.remove([item.id]) }
      }
    }
    .controlSize(.small)
  }
}
