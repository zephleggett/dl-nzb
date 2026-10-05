import DlNzbKit
import DlNzbUI
import SwiftUI

/// One download in the list: what it is, its name, one status line, and on
/// the trailing edge the App Store's ring (tap to pause or resume), a check
/// when it is done, or a retry button when it failed. Content, so no glass.
///
/// From xxLarge text the name and status get two lines; at accessibility
/// sizes the row stacks: the name gets three lines, the status three, the
/// buttons stack, and the control moves under them.
struct DownloadRowView: View {
  let item: DownloadItem
  @Binding var prompts: ItemPromptState

  @Environment(AppRuntime.self) private var runtime
  @Environment(\.dynamicTypeSize) private var dynamicTypeSize
  @ScaledMetric(relativeTo: .title2) private var iconWidth: CGFloat = 30

  private var isLarge: Bool { dynamicTypeSize.isAccessibilitySize }
  /// xxLarge and up: one line truncates too much of a name.
  private var isLargerText: Bool { dynamicTypeSize >= .xxLarge }

  /// The status line as the queue stands: "Paused" for a waiting row it holds back.
  private var status: String {
    RowStatus.line(for: item, held: runtime.queue.isHeld(item))
  }

  var body: some View {
    let layout = isLarge ? AnyLayout(VStackLayout(alignment: .leading, spacing: 8)) : AnyLayout(HStackLayout(alignment: .center, spacing: 12))
    layout {
      HStack(alignment: .center, spacing: 12) {
        if !isLarge {
          ContentKindIcon(item.contentKind)
            .font(.title2)
            .frame(width: iconWidth)
            // Said by the text beside it.
            .accessibilityHidden(true)
        }
        VStack(alignment: .leading, spacing: 3) {
          VStack(alignment: .leading, spacing: 3) {
            // Over more than one line, broken between the name's parts.
            Text(isLargerText ? ReleaseText.breakable(item.displayTitle) : item.displayTitle)
              .font(.body)
              .lineLimit(isLarge ? 3 : (isLargerText ? 2 : 1))
              .truncationMode(.middle)
            statusLine
          }
          // One element for what the row is and how it stands; the buttons
          // under it stay their own, so each can be found and tapped.
          .accessibilityElement(children: .ignore)
          .accessibilityLabel("\(item.contentKind.accessibilityName), \(ReleaseText.spoken(item.displayTitle)), \(status)")
          .accessibilityActions { attentionButtons }
          attentionActions
        }
        .frame(maxWidth: .infinity, alignment: .leading)
      }
      DownloadRowControl(item: item, prompts: $prompts)
    }
    .padding(.vertical, 4)
  }

  /// The shared status line in this row's wording, with a question given
  /// room to wrap.
  private var statusLine: some View {
    DownloadStatusLine(
      text: status, tone: StatusText.tone(for: item), lineLimit: item.needsAttention || isLarge ? 3 : (isLargerText ? 2 : 1)
    )
    .fixedSize(horizontal: false, vertical: item.needsAttention)
  }

  /// The two decisions a row can ask for, right where the question is; side
  /// by side, or stacked from xxLarge text, where two no longer fit a phone's row.
  @ViewBuilder private var attentionActions: some View {
    if hasAttentionActions {
      let layout = isLargerText ? AnyLayout(VStackLayout(alignment: .leading, spacing: 8)) : AnyLayout(HStackLayout(spacing: 8))
      layout { attentionButtons }
        .buttonStyle(.bordered)
        .controlSize(.small)
        .padding(.top, 4)
    }
  }

  private var hasAttentionActions: Bool {
    switch item.state {
    case .needsAttention(.unrepairable), .needsAttention(.password): true
    default: false
    }
  }

  @ViewBuilder private var attentionButtons: some View {
    switch item.state {
    case .needsAttention(.unrepairable):
      Button("Download Anyway") { runtime.downloadAnyway(item.id) }
      Button("Remove", role: .destructive) { runtime.requestRemove(item, prompts: &prompts) }
    case .needsAttention(.password):
      Button("Enter Password…") { prompts.ask(.password, about: item.id) }
    default:
      EmptyView()
    }
  }
}

/// The trailing control. A button whenever tapping it does something, with
/// a target the size of a fingertip.
struct DownloadRowControl: View {
  let item: DownloadItem
  @Binding var prompts: ItemPromptState
  @Environment(AppRuntime.self) private var runtime

  var body: some View {
    Group {
      switch item.state {
      case .queued, .running, .paused:
        // The ring does what it shows; with nothing to pause or resume
        // (post-processing), it stops.
        let action = runtime.primaryAction(for: item)
        ringButton(ring(for: action), label: action?.title ?? "Stop") {
          if let action {
            runtime.perform(action, on: item.id)
          } else {
            runtime.requestStop(item, prompts: &prompts)
          }
        }
      case .failed, .stopped, .needsAttention(.diskFull):
        Button {
          runtime.retry(item.id)
        } label: {
          Image(systemName: "arrow.clockwise.circle.fill")
            .symbolRenderingMode(.hierarchical)
            .font(.title)
            .foregroundStyle(.tint)
            .frame(minWidth: 44, minHeight: 44)
            .contentShape(.rect)
        }
        .buttonStyle(.borderless)
        .accessibilityLabel(StatusText.retryTitle(for: item))
      case .needsAttention(.password):
        Button {
          prompts.ask(.password, about: item.id)
        } label: {
          glyph
        }
        .buttonStyle(.borderless)
        .accessibilityLabel("Enter Password")
      case .finished, .needsAttention:
        glyph
      }
    }
  }

  private var glyph: some View {
    Group {
      if let symbol = RowStatus.symbol(for: item) {
        Image(systemName: symbol.name)
          .symbolRenderingMode(.hierarchical)
          .font(.title)
          .foregroundStyle(symbol.color)
      }
    }
    .frame(minWidth: 44, minHeight: 44)
    .accessibilityHidden(true)
  }

  private func ring(for action: PrimaryAction?) -> ProgressRing {
    guard item.isQueued else { return ProgressRing(item) }
    if action == .pause {
      // Waiting: drawn quietly, with a clock rather than the pause a
      // running ring shows, so the one ring that moves stands out.
      return ProgressRing(fraction: item.downloadFraction, glyph: .waiting, isPaused: true)
    }
    // Held back (Pause All, or a server problem) or waiting for Start: drawn
    // as paused, and a tap starts it, or tries the server again when that is
    // what holds it.
    return ProgressRing(fraction: item.downloadFraction, glyph: .resume, isPaused: true)
  }

  private func ringButton(_ ring: ProgressRing, label: String, action: @escaping () -> Void) -> some View {
    Button(action: action) {
      ring
        .frame(minWidth: 44, minHeight: 44)
        .contentShape(.rect)
    }
    .buttonStyle(.borderless)
    .accessibilityLabel(label)
    .accessibilityValue(item.downloadFraction > 0 ? Format.percent(item.downloadFraction) : "")
  }
}
