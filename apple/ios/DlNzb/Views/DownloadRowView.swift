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

  /// A line that changes several times a second: no animated transition,
  /// which would leave its digits forever half-blurred.
  private var statusLine: some View {
    Text(status)
      .font(.subheadline)
      .foregroundStyle(StatusText.tone(for: item).textStyle)
      .monospacedDigit()
      .lineLimit(item.needsAttention || isLarge ? 3 : (isLargerText ? 2 : 1))
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
      case .queued where runtime.queue.isHeld(item):
        // Held back (Pause All, or a server problem): drawn as paused, and a
        // tap starts it, or tries the server again when that is what holds it.
        ringButton(ProgressRing(fraction: item.downloadFraction, glyph: .resume, isPaused: true), label: "Resume") {
          runtime.resumeHeld(item.id)
        }
      case .queued where runtime.queue.awaitsStart(item):
        ringButton(ProgressRing(fraction: item.downloadFraction, glyph: .resume, isPaused: true), label: "Start") { runtime.start(item.id) }
      case .queued:
        // Waiting: drawn quietly, with a clock rather than the pause a
        // running ring shows, so the one ring that moves stands out.
        ringButton(ProgressRing(fraction: item.downloadFraction, glyph: .waiting, isPaused: true), label: "Pause") { runtime.pause(item.id) }
      case .running, .paused:
        let action = ringAction
        ringButton(ProgressRing(item), label: action.label, action: action.perform)
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

  /// What tapping the ring does, as `ProgressRing(item)` draws it.
  private var ringAction: (label: String, perform: () -> Void) {
    if item.canResume { return ("Resume", { runtime.resume(item.id) }) }
    if item.canPause { return ("Pause", { runtime.pause(item.id) }) }
    return ("Stop", { runtime.requestStop(item, prompts: &prompts) })
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
