import AppKit
import DlNzbKit
import DlNzbUI
import SwiftUI

/// The trailing inspector: the selected download's detail, or what to do
/// when there is no single selection.
struct InspectorPanel: View {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue

  var body: some View {
    let selected = app.items(app.selection)
    if selected.count == 1, let item = selected.first {
      LiveItem(item, in: queue) { InspectorView(item: $0) }
    } else {
      // Quiet, as Xcode's inspector is: a line of secondary text.
      Text(selected.isEmpty ? "No Selection" : "\(selected.count.formatted()) Downloads Selected")
        .font(.title3)
        .foregroundStyle(.secondary)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
  }
}

/// Name, size and state, with the row's question when it asks one; where it
/// goes, with Show in Finder; the phase checklist; the files; and the facts
/// worth knowing.
struct InspectorView: View {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue
  let item: DownloadItem

  var body: some View {
    Form {
      Section {
        VStack(alignment: .leading, spacing: 6) {
          HStack(alignment: .firstTextBaseline, spacing: 8) {
            ContentKindIcon(item.contentKind)
            // Not selectable: a copy would carry the invisible break
            // characters. Copy Name (⌘C) copies the plain name.
            Text(ReleaseText.breakable(item.displayTitle))
              .font(.headline)
              .lineLimit(5)
              .truncationMode(.middle)
              .fixedSize(horizontal: false, vertical: true)
              .accessibilityLabel(ReleaseText.spoken(item.displayTitle))
              .accessibilityAddTraits(.isHeader)
          }
          if let headline = StatusText.headline(for: item) {
            Text(headline)
              .font(.subheadline.weight(.semibold))
              .foregroundStyle(StatusText.tone(for: item).emphasisStyle)
          }
          // Room for the whole line here, which a row cuts to one.
          DownloadStatusLine(item, held: queue.isHeld(item), lineLimit: 4)
            .fixedSize(horizontal: false, vertical: true)
          if case .needsAttention(let attention) = item.state {
            AttentionActions(item: item, attention: attention)
              .padding(.top, 2)
          }
        }
        .padding(.vertical, 2)
      }

      Section("Destination") {
        HStack(spacing: 8) {
          FolderLabel(url: item.outputDirectory, namesParent: true)
            .frame(maxWidth: .infinity, alignment: .leading)
          RowButton("Show in Finder", systemImage: "magnifyingglass.circle.fill") { app.reveal([item.id]) }
        }
      }

      Section("Progress") {
        PhaseChecklist(item)
          .padding(.vertical, 2)
      }

      let files = InspectorFiles(item)
      if !files.rows.isEmpty {
        Section("Files") {
          ForEach(files.rows) { file in
            LabeledContent {
              Text(Format.bytes(file.bytes))
                .monospacedDigit()
            } label: {
              Label {
                Text(file.name)
                  .lineLimit(1)
                  .truncationMode(.middle)
                  .help(file.name)
              } icon: {
                Image(systemName: file.symbol)
                  .foregroundStyle(.secondary)
              }
            }
          }
          if let more = files.more {
            Text(more)
              .foregroundStyle(.secondary)
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
    }
    .formStyle(.grouped)
  }
}

/// A folder's name and where it is, with its full path as the tooltip. The
/// line under the name is left out when it would only say "~" (a folder in
/// the home folder, such as Downloads): the name says enough there.
struct FolderLabel: View {
  let url: URL
  /// The job's own folder is named after the release, which the inspector
  /// shows already; its parent says more.
  var namesParent = false

  var body: some View {
    let shown = namesParent ? url.deletingLastPathComponent() : url
    let location = shown.deletingLastPathComponent().abbreviatedPath
    Label {
      VStack(alignment: .leading, spacing: 1) {
        Text(shown.lastPathComponent)
          .lineLimit(1)
          .truncationMode(.middle)
        if location != "~" {
          Text(location)
            .font(.caption)
            .foregroundStyle(.secondary)
            .lineLimit(1)
            .truncationMode(.middle)
        }
      }
    } icon: {
      Image(systemName: "folder.fill")
        .foregroundStyle(.secondary)
    }
    .help(url.path(percentEncoded: false))
  }
}

/// The Files section: what the job left once finished, otherwise what the
/// NZB holds, with recovery files summed into one line.
struct InspectorFiles {
  struct Row: Identifiable {
    let name: String
    let bytes: Int64
    let symbol: String
    var id: String { name }
  }

  /// A RAR set can have a hundred volumes; past this many, one line says how
  /// many more.
  static let limit = 12

  let rows: [Row]
  let more: String?

  init(_ item: DownloadItem) {
    var rows = item.listedFiles.map { Row(name: $0.name, bytes: $0.bytes, symbol: Self.symbol(forName: $0.name)) }
    var extra: [String] = []
    // Until the job has left files of its own, the NZB's recovery files are one line.
    let recovery = item.outputFiles == nil ? (item.info?.files ?? []).filter { $0.kind == .par2 } : []
    if !recovery.isEmpty {
      let bytes = recovery.reduce(Int64(0)) { $0 + $1.bytes }
      extra.append("Recovery data · \(Format.count(recovery.count, "file", "files")) · \(Format.bytes(bytes))")
    }
    if rows.count > Self.limit {
      let hidden = rows.count - Self.limit
      rows = Array(rows.prefix(Self.limit))
      extra.insert("\(Format.count(hidden, "more file", "more files"))", at: 0)
    }
    self.rows = rows
    self.more = extra.isEmpty ? nil : extra.joined(separator: "\n")
  }

  /// Archive volumes (.rar, .r00, .001) as archives, anything else by its extension.
  static func symbol(forName name: String) -> String {
    if NzbFile.kind(forName: name) == .archive { return ContentKind.archive.symbolName }
    return (ContentKind.forExtension((name as NSString).pathExtension) ?? .other).symbolName
  }
}

extension URL {
  /// The path with the home folder as ~. The user's real home, not the
  /// sandbox container NSString's own abbreviation would use.
  var abbreviatedPath: String {
    var path = standardizedFileURL.path(percentEncoded: false)
    while path.count > 1 && path.hasSuffix("/") { path.removeLast() }
    var home = CLIConfig.realHomeDirectory.path(percentEncoded: false)
    while home.count > 1 && home.hasSuffix("/") { home.removeLast() }
    if path == home { return "~" }
    if path.hasPrefix(home + "/") { return "~" + path.dropFirst(home.count) }
    return path
  }
}
