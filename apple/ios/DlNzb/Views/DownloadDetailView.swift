import DlNzbKit
import DlNzbUI
import SwiftUI

/// Everything about one download, in the Mac inspector's sections: what it
/// is and what it is doing, where its files are, the phase checklist, the
/// files, and the details. It redraws with the progress; the sections that
/// show none of it are views of their own that compare the item without it
/// (`equalsIgnoringProgress`), so they are not drawn again with each update.
struct DownloadDetailView: View {
  let item: DownloadItem

  @State private var prompts = ItemPromptState()

  var body: some View {
    List {
      Section {
        DetailHeader(item: item, prompts: $prompts)
      }

      LocationSection(item: item)

      Section("Progress") {
        PhaseChecklist(item)
          .padding(.vertical, 4)
      }

      FilesSection(item: item)
      DetailsSection(item: item)

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
      // The whole line, which a row cuts short.
      DownloadStatusLine(item, held: runtime.queue.isHeld(item), lineLimit: .max)
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

  /// What moves it on (a held download reads "Paused", so it offers
  /// Resume), then Stop while it can stop, or the question's answers.
  @ViewBuilder private var actions: some View {
    if let action = runtime.primaryAction(for: item) {
      let button = Button(action.title, systemImage: action.systemImage) { runtime.perform(action, on: item.id) }
      if action == .pause {
        button.buttonStyle(.bordered)
      } else {
        button.buttonStyle(.borderedProminent)
      }
    }
    if item.canStop {
      stopButton
    }
    switch item.state {
    case .needsAttention(.unrepairable):
      Button("Download Anyway") { runtime.downloadAnyway(item.id) }
        .buttonStyle(.borderedProminent)
      Button("Remove", role: .destructive) { runtime.requestRemove(item, prompts: &prompts) }
        .buttonStyle(.bordered)
    case .needsAttention(.password):
      Button("Enter Password…") { prompts.ask(.password, about: item.id) }
        .buttonStyle(.borderedProminent)
    default:
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

/// Where its files are in the Files app, with Show in Files and Share.
private struct LocationSection: View, Equatable {
  let item: DownloadItem

  nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
    lhs.item.equalsIgnoringProgress(rhs.item)
  }

  var body: some View {
    let path = FilesLocation.displayPath(of: item.outputDirectory)
    Section("Location") {
      LabeledContent {
        EmptyView()
      } label: {
        Text(ReleaseText.breakable(path))
          .font(.subheadline)
          .foregroundStyle(.secondary)
          .accessibilityLabel(path)
      }
      FileActionButtons(item: item)
    }
  }
}

/// The first few files, biggest first, and the rest a tap away. Sorting a
/// hundred archive volumes is worth doing once, not with every progress
/// update.
private struct FilesSection: View, Equatable {
  let item: DownloadItem

  /// Enough files to see what the download is; the rest are one tap away.
  private let shownFiles = 6

  nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
    lhs.item.equalsIgnoringProgress(rhs.item)
  }

  var body: some View {
    let files = item.listedFiles
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
  }
}

/// The facts worth knowing (`DetailRow`), none of them progress.
private struct DetailsSection: View, Equatable {
  let item: DownloadItem

  nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
    lhs.item.equalsIgnoringProgress(rhs.item)
  }

  var body: some View {
    Section("Details") {
      ForEach(DetailRow.rows(for: item)) { row in
        LabeledContent(row.label) {
          Text(row.value)
            .monospacedDigit()
            .multilineTextAlignment(.trailing)
        }
      }
    }
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
