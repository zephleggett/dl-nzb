import DlNzbKit
import Foundation
import Observation

/// Where NZBs come in: `.onOpenURL` (Files, Safari's downloads, the share
/// sheet, AirDrop, Mail) and the [+] button's file importer, which all end up
/// in `DownloadQueue.add(urls:)`.
///
/// The queue reads each file under its security scope and keeps its own copy,
/// so an NZB opened in place (iCloud Drive, On My iPhone) is never touched
/// again. Apps that cannot share in place hand over a copy in Documents/Inbox,
/// which the Files app would otherwise show under dl-nzb for ever; that copy
/// is deleted once read.
@MainActor
@Observable
final class OpenRouter {
  struct Failure {
    let fileName: String
    let message: String
  }

  /// NZBs already in the list or on disk, asked about one at a time.
  private(set) var duplicates: [DuplicateNZB] = []
  /// Files that could not be added, shown together.
  var failures: [Failure] = []
  /// The newest item added, for the iPad to select.
  private(set) var lastAdded: DownloadItem.ID?

  @ObservationIgnored private let open: @MainActor ([URL]) async -> [AddResult]
  @ObservationIgnored private let inboxDirectory: URL

  init(inboxDirectory: URL = OpenRouter.defaultInbox, open: @escaping @MainActor ([URL]) async -> [AddResult]) {
    self.inboxDirectory = inboxDirectory
    self.open = open
  }

  /// Documents/Inbox, where iOS puts copies of files other apps send.
  static var defaultInbox: URL {
    URL.documentsDirectory.appending(path: "Inbox", directoryHint: .isDirectory)
  }

  /// The duplicate being asked about now.
  var duplicate: DuplicateNZB? { duplicates.first }

  /// Adds what can be added. Anything that is not a file is ignored: dl-nzb
  /// declares no URL scheme, so only documents arrive here.
  @discardableResult
  func handle(_ urls: [URL]) async -> [AddResult] {
    let files = urls.filter(\.isFileURL)
    for url in urls where !url.isFileURL {
      AppLog.open.notice("ignored a URL that is not a file: \(url.scheme ?? "no scheme", privacy: .public)")
    }
    guard !files.isEmpty else { return [] }
    AppLog.open.info("opening \(files.count) file\(files.count == 1 ? "" : "s", privacy: .public)")
    let results = await open(files)
    removeInboxCopies(of: files)
    for result in results {
      switch result {
      case .added(let id):
        lastAdded = id
      case .duplicate(let duplicate):
        duplicates.append(duplicate)
      case .failed(let fileName, let message):
        failures.append(Failure(fileName: fileName, message: message))
      }
    }
    return results
  }

  /// The current duplicate was answered (Download Again, Show, Cancel).
  func dismissDuplicate() {
    if !duplicates.isEmpty { duplicates.removeFirst() }
  }

  /// One alert for every failure so far.
  var failureTitle: String {
    failures.count == 1 ? "Couldn’t Open “\(failures[0].fileName)”" : "Couldn’t Open \(failures.count) Files"
  }

  var failureMessage: String {
    failures.count == 1 ? failures[0].message : failures.map { "\($0.fileName): \($0.message)" }.joined(separator: "\n")
  }

  // MARK: Inbox

  func isInInbox(_ url: URL) -> Bool {
    let inbox = inboxDirectory.resolvingSymlinksInPath().standardizedFileURL.path(percentEncoded: false)
    let path = url.resolvingSymlinksInPath().standardizedFileURL.path(percentEncoded: false)
    let prefix = inbox.hasSuffix("/") ? inbox : inbox + "/"
    return path.hasPrefix(prefix)
  }

  private func removeInboxCopies(of urls: [URL]) {
    for url in urls where isInInbox(url) {
      do {
        try FileManager.default.removeItem(at: url)
      } catch {
        AppLog.open.error("could not remove \(url.lastPathComponent, privacy: .public) from the inbox: \(error.localizedDescription, privacy: .public)")
      }
    }
  }
}
