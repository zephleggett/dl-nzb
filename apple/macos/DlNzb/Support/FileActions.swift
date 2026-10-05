import AppKit
import DlNzbKit

/// Finder's side of a download: what Show in Finder selects, what Open
/// opens, and the panels that pick files and folders.
@MainActor
enum FileActions {
  /// Show in Finder selects the main file of a finished download, its folder
  /// while it runs, or the download folder when the job folder is not there yet.
  static func revealTarget(_ item: DownloadItem) -> URL {
    if item.isFinished, let file = mainFile(of: item), exists(file) { return file }
    if exists(item.outputDirectory) { return item.outputDirectory }
    return item.outputDirectory.deletingLastPathComponent()
  }

  /// Open opens the file when one file is the download (a film, an ISO) and
  /// the folder when it is a set of files.
  static func openTarget(for item: DownloadItem) -> URL {
    guard let summary = item.summary, let main = mainFile(of: item) else { return item.outputDirectory }
    let total = summary.files.reduce(Int64(0)) { $0 + $1.bytes }
    let mainBytes = summary.files.map(\.bytes).max() ?? 0
    // One file holding nearly everything; sidecars (nfo, sfv) do not count.
    return total > 0 && Double(mainBytes) / Double(total) >= 0.9 && exists(main) ? main : item.outputDirectory
  }

  /// Quick Look shows a finished download's main file, or its folder.
  static func previewTarget(_ item: DownloadItem) -> URL {
    mainFile(of: item) ?? item.outputDirectory
  }

  /// The largest file a finished job left.
  static func mainFile(of item: DownloadItem) -> URL? {
    guard let summary = item.summary, let largest = summary.files.max(by: { $0.bytes < $1.bytes }) else { return nil }
    return summary.outputDirectory.appending(path: largest.name, directoryHint: .notDirectory)
  }

  static func reveal(_ urls: [URL]) {
    guard !urls.isEmpty else { return }
    NSWorkspace.shared.activateFileViewerSelecting(urls)
  }

  static func open(_ url: URL) {
    NSWorkspace.shared.open(url)
  }

  private static func exists(_ url: URL) -> Bool {
    FileManager.default.fileExists(atPath: url.path(percentEncoded: false))
  }

  // MARK: Panels

  /// Choose… for the download folder.
  static func chooseDownloadFolder(startingAt folder: URL) -> URL? {
    let panel = NSOpenPanel()
    panel.canChooseDirectories = true
    panel.canChooseFiles = false
    panel.canCreateDirectories = true
    panel.allowsMultipleSelection = false
    panel.directoryURL = folder
    panel.prompt = "Choose"
    panel.message = "Choose where dl-nzb puts downloads. Each download gets a folder of its own."
    return panel.runModal() == .OK ? panel.url : nil
  }

  /// Import from dl-nzb CLI…: the sandbox cannot read the CLI's settings
  /// until the user picks the file, so the panel starts where it lives.
  static func chooseCLIConfig() -> URL? {
    let panel = NSOpenPanel()
    panel.canChooseFiles = true
    panel.canChooseDirectories = false
    panel.allowsMultipleSelection = false
    panel.showsHiddenFiles = false
    // A file URL here opens its folder with the file selected, so Import is
    // one click when the CLI's config.toml is where it usually is.
    panel.directoryURL = CLIConfig.defaultURL
    panel.prompt = "Import"
    panel.message = "Choose the dl-nzb settings file, config.toml, to use its server and options."
    return panel.runModal() == .OK ? panel.url : nil
  }

  /// Whether the dl-nzb CLI's settings file may be there, for the SPEC's rule
  /// that the import is offered only when it exists. The sandbox may refuse to
  /// even look (EPERM); only a definite "no such file" hides the offer.
  static var cliConfigMayExist: Bool {
    guard let url = CLIConfig.defaultURL else { return false }
    var info = stat()
    if stat(url.path(percentEncoded: false), &info) == 0 { return true }
    return errno != ENOENT && errno != ENOTDIR
  }

  /// A one-off error, from Settings or the onboarding sheet.
  static func showError(_ title: String, _ message: String) {
    let alert = NSAlert()
    alert.alertStyle = .warning
    alert.messageText = title
    alert.informativeText = message
    alert.runModal()
  }
}

extension MacApp {
  /// Choose… in General: a security-scoped bookmark the settings keep.
  func chooseDownloadFolder() {
    guard let url = FileActions.chooseDownloadFolder(startingAt: settings.downloadFolder) else { return }
    do {
      try settings.setDownloadFolder(url)
    } catch {
      FileActions.showError("Couldn’t Use That Folder", "dl-nzb could not keep access to \(url.lastPathComponent). Choose another folder.")
    }
  }

  /// Import from dl-nzb CLI… in Advanced and the onboarding sheet.
  func importCLIConfig() {
    guard let url = FileActions.chooseCLIConfig() else { return }
    Task {
      do {
        try await model.importCLIConfig(from: url)
      } catch {
        let message = (error as? EngineError)?.message ?? error.localizedDescription
        FileActions.showError("Couldn’t Import Settings", message)
      }
    }
  }
}
