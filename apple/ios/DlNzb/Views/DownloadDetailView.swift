import DlNzbKit
import DlNzbUI
import SwiftUI

/// Everything about one download, in the Mac inspector's sections: what it
/// is and what it is doing, where its files are, the phase checklist, the
/// files, and the details.
struct DownloadDetailView: View {
  let item: DownloadItem

  @State private var prompts = ItemPromptState()

  /// Enough files to see what the download is; the rest are one tap away.
  private let shownFiles = 6

  var body: some View {
    // Sorted once per update: a release can list a hundred files, and the
    // item changes several times a second while it downloads.
    let files = item.listedFiles
    List {
      Section {
        DetailHeader(item: item, prompts: $prompts)
      }

      Section("Location") {
        LabeledContent {
          EmptyView()
        } label: {
          Text(ReleaseText.breakable(FilesLocation.displayPath(of: item.outputDirectory)))
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .accessibilityLabel(FilesLocation.displayPath(of: item.outputDirectory))
        }
        FileActionButtons(item: item)
      }

      Section("Progress") {
        PhaseChecklist(item)
          .padding(.vertical, 4)
      }

      if !files.isEmpty {
        Section("Files") {
          ForEach(files.prefix(shownFiles), id: \.self) { file in
            FileRow(file: file)
          }
          if files.count > shownFiles {
            NavigationLink("All \(Format.count(files.count)) Files") {
              FileList(files: files)
            }
          }
        }
      }

      Section("Details") {
        ForEach(DetailRow.rows(for: item)) { row in
          LabeledContent(row.label) {
            Text(row.value)
              .monospacedDigit()
              .multilineTextAlignment(.trailing)
          }
        }
      }

      if !item.warnings.isEmpty {
        Section("Warnings") {
          ForEach(Array(item.warnings.enumerated()), id: \.offset) { _, warning in
            Text(warning)
              .font(.subheadline)
          }
        }
      }
    }
    .listStyle(.insetGrouped)
    .readableWidth()
    .navigationTitle(item.displayTitle)
    .navigationBarTitleDisplayMode(.inline)
    // The header shows the name in full; truncated in the bar it says less.
    .toolbar(removing: .title)
    .toolbar {
      ToolbarItem(placement: .topBarTrailing) {
        Menu {
          DownloadMenu(item: item, prompts: $prompts)
        } label: {
          Label("Actions", systemImage: "ellipsis")
        }
      }
    }
    .itemPrompts(state: $prompts)
  }
}

/// What it is and where it stands, the name in full, the status line, the
/// bar, and the buttons that move it on.
private struct DetailHeader: View {
  let item: DownloadItem
  @Binding var prompts: ItemPromptState
  @Environment(AppRuntime.self) private var runtime
  @Environment(\.dynamicTypeSize) private var dynamicTypeSize

  /// At accessibility text sizes, side by side would break words in two.
  private var isLarge: Bool { dynamicTypeSize.isAccessibilitySize }

  var body: some View {
    VStack(alignment: .leading, spacing: 10) {
      // Kind and state side by side; at accessibility sizes, one above the
      // other rather than broken mid-word.
      if isLarge {
        VStack(alignment: .leading, spacing: 4) {
          kind
          state
        }
        .font(.subheadline)
      } else {
        HStack(alignment: .firstTextBaseline) {
          kind
          Spacer(minLength: 8)
          state
        }
        .font(.subheadline)
      }
      // Broken between its parts rather than hyphenated; Copy Name, in the
      // menu, copies the name itself.
      Text(ReleaseText.breakable(item.displayTitle))
        .font(.title3.weight(.semibold))
        .fixedSize(horizontal: false, vertical: true)
        .accessibilityLabel(ReleaseText.spoken(item.displayTitle))
        .accessibilityAddTraits(.isHeader)
      DownloadProgressBar(item)
      // Updated several times a second: no animated transition, which would
      // keep its digits blurred.
      Text(StatusText.line(for: item, in: runtime.queue))
        .font(.subheadline)
        .foregroundStyle(StatusText.tone(for: item).textStyle)
        .monospacedDigit()
        .fixedSize(horizontal: false, vertical: true)
      if hasActions {
        let layout = isLarge ? AnyLayout(VStackLayout(alignment: .leading, spacing: 10)) : AnyLayout(HStackLayout(spacing: 10))
        layout { actions }
          .padding(.top, 4)
      }
    }
    .padding(.vertical, 6)
  }

  // Both wrap rather than truncate once stacked.
  private var kind: some View {
    Label(item.contentKind.accessibilityName, systemImage: item.contentKind.symbolName)
      .foregroundStyle(.secondary)
      .fixedSize(horizontal: false, vertical: true)
  }

  private var state: some View {
    Text(RowStatus.title(for: item, awaitsStart: runtime.queue.awaitsStart(item), held: runtime.queue.isHeld(item)))
      .fontWeight(.semibold)
      .foregroundStyle(StatusText.tone(for: item).emphasisStyle)
      .fixedSize(horizontal: false, vertical: true)
  }

  private var hasActions: Bool {
    if case .finished = item.state { return false }
    return true
  }

  @ViewBuilder private var actions: some View {
    switch item.state {
    case .queued where runtime.queue.isHeld(item):
      // Reads "Paused", so it offers what a paused download does.
      Button("Resume", systemImage: "play.fill") { runtime.resumeHeld(item.id) }
        .buttonStyle(.borderedProminent)
      stopButton
    case .queued where runtime.queue.awaitsStart(item):
      Button("Start", systemImage: "arrow.down") { runtime.start(item.id) }
        .buttonStyle(.borderedProminent)
      stopButton
    case .queued, .running, .paused:
      if item.canResume {
        Button("Resume", systemImage: "play.fill") { runtime.resume(item.id) }
          .buttonStyle(.borderedProminent)
      } else if item.canPause {
        Button("Pause", systemImage: "pause.fill") { runtime.pause(item.id) }
          .buttonStyle(.bordered)
      }
      stopButton
    case .failed, .stopped, .needsAttention(.diskFull):
      Button(StatusText.retryTitle(for: item), systemImage: "arrow.clockwise") { runtime.retry(item.id) }
        .buttonStyle(.borderedProminent)
    case .needsAttention(.unrepairable):
      Button("Download Anyway") { runtime.downloadAnyway(item.id) }
        .buttonStyle(.borderedProminent)
      Button("Remove", role: .destructive) { runtime.requestRemove(item, prompts: &prompts) }
        .buttonStyle(.bordered)
    case .needsAttention(.password):
      Button("Enter Password…") { prompts.ask(.password, about: item.id) }
        .buttonStyle(.borderedProminent)
    case .finished:
      EmptyView()
    }
  }

  private var stopButton: some View {
    Button(role: .destructive) {
      runtime.requestStop(item, prompts: &prompts)
    } label: {
      // Spelled out: in a list row the icon would otherwise take the accent.
      Label("Stop", systemImage: "stop.fill")
        .foregroundStyle(.red)
    }
    .buttonStyle(.bordered)
    .tint(.red)
  }
}

/// A file's name on one line, truncated in the middle so its extension
/// shows, and its size.
private struct FileRow: View {
  let file: OutputFile
  @Environment(\.dynamicTypeSize) private var dynamicTypeSize

  var body: some View {
    HStack(alignment: .firstTextBaseline, spacing: 12) {
      Text(file.name)
        .lineLimit(dynamicTypeSize.isAccessibilitySize ? 3 : 1)
        .truncationMode(.middle)
      Spacer(minLength: 0)
      Text(Format.bytes(file.bytes))
        .foregroundStyle(.secondary)
        .monospacedDigit()
        .fixedSize()
    }
    .accessibilityElement(children: .combine)
  }
}

/// Every file, for a release with dozens of archive volumes.
private struct FileList: View {
  let files: [OutputFile]

  var body: some View {
    List(files, id: \.self) { file in
      FileRow(file: file)
    }
    .listStyle(.insetGrouped)
    .readableWidth()
    .navigationTitle("Files")
    .navigationBarTitleDisplayMode(.inline)
  }
}
