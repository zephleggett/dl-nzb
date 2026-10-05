import DlNzbKit
import SwiftUI

// The pieces of a download row that both apps compose: the Mac adds trailing
// inline buttons and a context menu, the iPhone a progress ring and swipe
// actions. Rows are content, so nothing here uses Liquid Glass: standard
// materials, semantic colours and the system accent only.

/// The release name on one line, truncated in the middle so both the start of
/// the name and the group at its end stay readable. VoiceOver reads it as
/// words (`ReleaseText.spoken`).
public struct DownloadTitle: View {
  let title: String

  public init(_ title: String) {
    self.title = title
  }

  public var body: some View {
    Text(title)
      .lineLimit(1)
      .truncationMode(.middle)
      .help(title)
      .accessibilityLabel(ReleaseText.spoken(title))
  }
}

/// The thin bar under the title, or nothing when the SPEC says none. On a
/// selected row the fill turns white, as the accent it would be drawn in is
/// the selection's own colour.
public struct DownloadProgressBar: View {
  let progress: RowProgress
  @Environment(\.backgroundProminence) private var backgroundProminence

  public init(_ item: DownloadItem) {
    self.progress = RowProgress.of(item)
  }

  private var isOnSelection: Bool { backgroundProminence == .increased }

  public var body: some View {
    switch progress {
    case .none:
      EmptyView()
    case .indeterminate:
      ProgressView()
        .progressViewStyle(.linear)
        .controlSize(.small)
        .tint(isOnSelection ? .white : nil)
    case .determinate(let fraction):
      ProgressView(value: fraction)
        .progressViewStyle(.linear)
        .controlSize(.small)
        .tint(isOnSelection ? .white : nil)
        .animation(.smooth(duration: 0.3), value: fraction)
    case .frozen(let fraction):
      ProgressView(value: fraction)
        .progressViewStyle(.linear)
        .controlSize(.small)
        .tint(isOnSelection ? .white.opacity(0.7) : .secondary)
    }
  }
}

/// The status line: digits keep their width and only problems are tinted,
/// in a shade that stays readable (`StatusTone.textStyle`). A problem is a
/// sentence the user has to read ("9% of articles are missing and there is
/// not enough recovery data"), so it may take a second line; progress stays
/// on one, unless the caller has room for more (`lineLimit`).
///
/// The numbers change in place. The engine reports four times a second, so a
/// rolling (`numericText`) transition never settles: every digit, changed or
/// not, would spend most of its time mid-roll and blurred.
public struct DownloadStatusLine: View {
  let text: String
  let tone: StatusTone
  let lineLimit: Int?

  /// - Parameters:
  ///   - held: The queue is holding the item back (`DownloadQueue.isHeld`),
  ///     so a waiting row reads "Paused".
  ///   - lineLimit: Lines the text may take; by default one, or two for a
  ///     problem. `.max` for the whole line, however long.
  public init(_ item: DownloadItem, held: Bool = false, lineLimit: Int? = nil) {
    self.text = StatusText.line(for: item, held: held)
    self.tone = StatusText.tone(for: item)
    self.lineLimit = lineLimit
  }

  /// A line of the app's own wording (the iPhone's shorter row line), as
  /// the item's would be drawn.
  public init(text: String, tone: StatusTone = .neutral, lineLimit: Int? = nil) {
    self.text = text
    self.tone = tone
    self.lineLimit = lineLimit
  }

  public var body: some View {
    Text(text)
      .font(.subheadline)
      .foregroundStyle(tone.textStyle)
      .monospacedDigit()
      .lineLimit(lineLimit ?? (tone == .warning || tone == .bad ? 2 : 1))
      .truncationMode(.tail)
  }
}

/// What the download is, as an SF Symbol: film, music, archive, document.
public struct ContentKindIcon: View {
  let kind: ContentKind

  public init(_ kind: ContentKind) {
    self.kind = kind
  }

  public var body: some View {
    Image(systemName: kind.symbolName)
      .symbolRenderingMode(.hierarchical)
      .foregroundStyle(.secondary)
      .accessibilityLabel(kind.accessibilityName)
  }
}

/// The glyph for a finished, failed, stopped or stuck download, coloured by
/// its tone; nothing while it runs or waits.
public struct StatusGlyph: View {
  let symbol: (name: String, tone: StatusTone)?

  public init(_ item: DownloadItem) {
    self.symbol = StatusSymbol.of(item)
  }

  public var body: some View {
    if let symbol {
      Image(systemName: symbol.name)
        .symbolRenderingMode(.hierarchical)
        .foregroundStyle(symbol.tone.glyphStyle)
        .accessibilityHidden(true)
    }
  }
}

/// Icon, title, bar and status line: the body of a row on either platform.
/// The app puts its own controls beside it.
public struct DownloadRowContent: View {
  let item: DownloadItem
  let showsIcon: Bool
  let held: Bool

  /// - Parameter held: The queue is holding the item back
  ///   (`DownloadQueue.isHeld`), so a waiting row reads "Paused".
  public init(_ item: DownloadItem, showsIcon: Bool = true, held: Bool = false) {
    self.item = item
    self.showsIcon = showsIcon
    self.held = held
  }

  public var body: some View {
    HStack(alignment: .center, spacing: 10) {
      if showsIcon {
        ContentKindIcon(item.contentKind)
          .font(.title2)
          .frame(width: 28)
      }
      VStack(alignment: .leading, spacing: 4) {
        HStack(spacing: 6) {
          DownloadTitle(item.displayTitle)
          StatusGlyph(item)
            .imageScale(.small)
        }
        DownloadProgressBar(item)
        DownloadStatusLine(item, held: held)
      }
    }
    .padding(.vertical, 2)
    .accessibilityElement(children: .combine)
  }
}

#Preview("Every state") {
  List(PreviewData.allStates) { item in
    DownloadRowContent(item)
  }
  .frame(width: 520, height: 900)
}

#Preview("Downloading") {
  List {
    DownloadRowContent(PreviewData.downloading)
    DownloadRowContent(PreviewData.paused)
  }
  .frame(width: 420, height: 160)
}

#Preview("Finished and failed") {
  List {
    DownloadRowContent(PreviewData.finished)
    DownloadRowContent(PreviewData.finishedWithIssues)
    DownloadRowContent(PreviewData.failed)
  }
  .frame(width: 420, height: 220)
}

#Preview("Needs attention") {
  List {
    DownloadRowContent(PreviewData.needsAttentionUnrepairable)
    DownloadRowContent(PreviewData.needsPassword)
    DownloadRowContent(PreviewData.needsSpace)
  }
  .frame(width: 420, height: 220)
}
