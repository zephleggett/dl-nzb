import AppIntents
import DlNzbKit
import Foundation
import UniformTypeIdentifiers

/// Download NZB, Pause All Downloads and Resume All Downloads, for Shortcuts,
/// Spotlight and Siri. They act on the running app's queue (`MacApp.shared`);
/// if dl-nzb is not open, the system launches it to run them.

struct DownloadNZBIntent: AppIntent {
  static let title: LocalizedStringResource = "Download NZB"
  static let description = IntentDescription("Adds an NZB file to dl-nzb. It downloads when its turn comes.")

  // Spelled out: the App Intents metadata extractor reads the identifier
  // from the source and cannot follow `UTType.nzb`.
  @Parameter(title: "NZB File", supportedContentTypes: [UTType(importedAs: "com.zephleggett.dl-nzb.nzb")])
  var file: IntentFile

  static var parameterSummary: some ParameterSummary {
    Summary("Download \(\.$file)")
  }

  @MainActor
  func perform() async throws -> some IntentResult & ProvidesDialog {
    let reply = try await IntentRouter(model: MacApp.shared.model).download(data: file.data, fileName: file.filename)
    return .result(dialog: "\(reply)")
  }
}

struct PauseAllIntent: AppIntent {
  static let title: LocalizedStringResource = "Pause All Downloads"
  static let description = IntentDescription("Pauses every download in dl-nzb until you resume them.")

  @MainActor
  func perform() async throws -> some IntentResult & ProvidesDialog {
    .result(dialog: "\(IntentRouter(model: MacApp.shared.model).pauseAll())")
  }
}

struct ResumeAllIntent: AppIntent {
  static let title: LocalizedStringResource = "Resume All Downloads"
  static let description = IntentDescription("Resumes every paused download in dl-nzb.")

  @MainActor
  func perform() async throws -> some IntentResult & ProvidesDialog {
    .result(dialog: "\(IntentRouter(model: MacApp.shared.model).resumeAll())")
  }
}

struct DlNzbShortcuts: AppShortcutsProvider {
  static var appShortcuts: [AppShortcut] {
    AppShortcut(
      intent: DownloadNZBIntent(), phrases: ["Download an NZB with \(.applicationName)", "Add an NZB to \(.applicationName)"],
      shortTitle: "Download NZB", systemImageName: "arrow.down.circle")
    AppShortcut(
      intent: PauseAllIntent(), phrases: ["Pause \(.applicationName)", "Pause downloads in \(.applicationName)"], shortTitle: "Pause All",
      systemImageName: "pause.circle")
    AppShortcut(
      intent: ResumeAllIntent(), phrases: ["Resume \(.applicationName)", "Resume downloads in \(.applicationName)"], shortTitle: "Resume All",
      systemImageName: "play.circle")
  }
}

/// What the intents do, apart from App Intents so the tests can drive it.
@MainActor
struct IntentRouter {
  struct Failure: LocalizedError {
    let message: String
    var errorDescription: String? { message }
  }

  let model: AppModel

  /// Adds the file's bytes as an NZB. The queue copies it, so the temporary
  /// file goes as soon as it is added.
  func download(data: Data, fileName: String) async throws -> String {
    let name = fileName.isEmpty ? "Download.nzb" : fileName
    let folder = FileManager.default.temporaryDirectory.appending(path: "Intents-\(UUID().uuidString)", directoryHint: .isDirectory)
    let url = folder.appending(path: name)
    do {
      try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
      try data.write(to: url)
    } catch {
      throw Failure(message: "dl-nzb could not read \(name).")
    }
    defer { try? FileManager.default.removeItem(at: folder) }
    let result = await model.open([url]).first
    switch result {
    case .added(let id):
      let title = model.queue.item(id)?.title ?? name
      return "Added “\(title)” to dl-nzb."
    case .duplicate(let duplicate):
      return "“\(duplicate.title)” is in dl-nzb already."
    case .failed(_, let message):
      throw Failure(message: message)
    case nil:
      throw Failure(message: "dl-nzb could not read \(name).")
    }
  }

  func pauseAll() -> String {
    model.launch()
    model.queue.pauseAll()
    return "Downloads are paused."
  }

  func resumeAll() -> String {
    model.launch()
    model.queue.resumeAll()
    return "Downloads are resuming."
  }
}
