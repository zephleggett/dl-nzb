import AppKit
import DlNzbKit
import OSLog

/// Show Logs: what dl-nzb has logged since it launched, written to
/// ~/Library/Logs/dl-nzb/dl-nzb.log (inside the sandbox's container) and
/// opened in Console, so a bug report can attach one file. Console is named
/// outright: a plain open from the sandbox does not reach it. If it cannot be
/// opened, the file is shown in Finder instead.
enum LogExport {
  static let console = URL(filePath: "/System/Applications/Utilities/Console.app", directoryHint: .isDirectory)

  @MainActor
  static func show() {
    Task {
      do {
        let file = try await Task.detached(priority: .userInitiated) { try write() }.value
        await open(file)
      } catch {
        Log.mac.error("the log could not be written: \(error.localizedDescription, privacy: .public)")
        FileActions.showError("Couldn’t Show Logs", "dl-nzb could not read its log. Console shows it under the subsystem com.zephleggett.dl-nzb.")
      }
    }
  }

  @MainActor
  private static func open(_ file: URL) async {
    do {
      _ = try await NSWorkspace.shared.open([file], withApplicationAt: console, configuration: NSWorkspace.OpenConfiguration())
    } catch {
      Log.mac.error("Console did not open the log: \(error.localizedDescription, privacy: .public)")
      NSWorkspace.shared.activateFileViewerSelecting([file])
    }
  }

  /// Every entry from this process under the app's subsystem, oldest first.
  private static func write() throws -> URL {
    let store = try OSLogStore(scope: .currentProcessIdentifier)
    let predicate = NSPredicate(format: "subsystem == %@", Log.subsystem)
    let entries = try store.getEntries(at: store.position(timeIntervalSinceLatestBoot: 0), matching: predicate)
    let time = Date.ISO8601FormatStyle(includingFractionalSeconds: true).year().month().day().time(includingFractionalSeconds: true)
    var text = ""
    for case let entry as OSLogEntryLog in entries {
      text += "\(entry.date.formatted(time)) [\(entry.category)] \(level(entry.level)) \(entry.composedMessage)\n"
    }
    let folder = try FileManager.default.url(for: .libraryDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
      .appending(path: "Logs/dl-nzb", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    let file = folder.appending(path: "dl-nzb.log")
    try Data(text.utf8).write(to: file, options: .atomic)
    return file
  }

  private static func level(_ level: OSLogEntryLog.Level) -> String {
    switch level {
    case .debug: "debug"
    case .info: "info"
    case .notice: "notice"
    case .error: "error"
    case .fault: "fault"
    default: "log"
    }
  }
}
