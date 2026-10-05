import DlNzbKit
import Foundation
import UserNotifications

// The words both apps use outside a row: release names that wrap or are
// spoken, the version line, the queue-wide alerts and the notifications. Each
// app lays them out its own way (alert buttons, notification actions); what
// they say is the same.

/// Release names on screen and to VoiceOver.
public enum ReleaseText {
  /// A release name with a zero-width space after each dot and underscore,
  /// so a wrapped name breaks between its parts ("Tears.of.Steel.") rather
  /// than being hyphenated mid-word, which would add hyphens the name lacks.
  /// Only for text drawn on screen: copying takes the name itself, VoiceOver
  /// `spoken(_:)`, and alerts and notifications the plain name.
  public static func breakable(_ name: String) -> String {
    var result = ""
    result.reserveCapacity(name.count + 16)
    for character in name {
      result.append(character)
      if character == "." || character == "_" { result.append("\u{200B}") }
    }
    return result
  }

  /// A release name as VoiceOver should read it: the dots and underscores
  /// between words become spaces, so "Tears.of.Steel.2012.1080p" is read as
  /// words rather than "Tears dot of dot Steel". A dot between two numbers
  /// stays ("5.1", "12.7.0"). For `accessibilityLabel`s of release names.
  public static func spoken(_ name: String) -> String {
    let parts = name.replacingOccurrences(of: "_", with: " ").split(separator: ".", omittingEmptySubsequences: false)
    var result = ""
    for (index, part) in parts.enumerated() {
      if index > 0 {
        let before = parts[index - 1].reversed().prefix { $0.isLetter || $0.isNumber }
        let after = part.prefix { $0.isLetter || $0.isNumber }
        let numbers = !before.isEmpty && !after.isEmpty && before.allSatisfy(\.isNumber) && after.allSatisfy(\.isNumber)
        result.append(numbers ? "." : " ")
      }
      result.append(contentsOf: part)
    }
    return result.split(separator: " ", omittingEmptySubsequences: true).joined(separator: " ")
  }
}

/// The app's version, as Settings shows it.
public enum AppVersion {
  /// "0.8.0 (123)".
  public static var display: String {
    let info = Bundle.main.infoDictionary ?? [:]
    let version = info["CFBundleShortVersionString"] as? String ?? "0.0.0"
    let build = info["CFBundleVersion"] as? String ?? "1"
    return "\(version) (\(build))"
  }
}

/// The alerts about the queue as a whole.
public enum AlertText {
  /// What went wrong with the server, in a few words.
  public static func serverProblemTitle(_ kind: EngineError.Kind?) -> String {
    switch kind {
    case .auth: "Couldn’t Log In to the Server"
    case .tls: "Couldn’t Connect Securely"
    case .dns: "Server Not Found"
    default: "Couldn’t Reach the Server"
    }
  }

  /// The engine's sentence, and what it means for the queue.
  public static func serverProblemMessage(_ problem: EngineError) -> String {
    "\(problem.message) Downloads are paused until the server works again."
  }

  /// "Already in the List" while the same NZB is still to finish there,
  /// "Already Downloaded" once its files are in place.
  @MainActor
  public static func duplicateTitle(_ duplicate: DuplicateNZB, in queue: DownloadQueue) -> String {
    isWaiting(duplicate, in: queue) ? "Already in the List" : "Already Downloaded"
  }

  @MainActor
  public static func duplicateMessage(_ duplicate: DuplicateNZB, in queue: DownloadQueue) -> String {
    // Spoken form: an alert wraps a dotted release name mid-word otherwise.
    let name = ReleaseText.spoken(duplicate.title)
    if isWaiting(duplicate, in: queue) { return "“\(name)” is in the list already." }
    return "“\(name)” has been downloaded before. Its files are in the \(duplicate.folder.deletingLastPathComponent().lastPathComponent) folder."
  }

  /// The alert's second button: "Show in List" selects the earlier copy
  /// while it is still to finish (its folder may not exist yet), "Show in
  /// Finder" shows the folder of one already downloaded.
  @MainActor
  public static func duplicateShowTitle(_ duplicate: DuplicateNZB, in queue: DownloadQueue) -> String {
    isWaiting(duplicate, in: queue) ? "Show in List" : "Show in Finder"
  }

  @MainActor
  private static func isWaiting(_ duplicate: DuplicateNZB, in queue: DownloadQueue) -> Bool {
    queue.listedItem(for: duplicate) != nil
  }

  /// "Couldn’t Open “notes.nzb”", or "Couldn’t Open 3 Files" for several.
  public static func openFailureTitle(_ failures: [OpenFailure], locale: Locale = .autoupdatingCurrent) -> String {
    failures.count == 1 ? "Couldn’t Open “\(failures[0].fileName)”" : "Couldn’t Open \(Format.count(failures.count, locale: locale)) Files"
  }
}

/// A file that could not be added, and why, for the alert that lists them.
public struct OpenFailure: Equatable, Sendable {
  public let fileName: String
  public let message: String

  public init(fileName: String, message: String) {
    self.fileName = fileName
    self.message = message
  }
}

/// What a notification about an item says: the title what happened, the body
/// the release with its size once finished, or with the problem otherwise.
public struct NotificationText: Equatable, Sendable {
  public let title: String
  public let body: String

  /// Nil while the item waits, runs or was stopped: nothing to say. The
  /// name is plain: a notification wraps on its own, and is read aloud.
  public init?(_ item: DownloadItem, locale: Locale = .autoupdatingCurrent) {
    let name = item.displayTitle
    switch item.state {
    case .finished(let summary):
      title = summary.outcome == .completed ? "Download Finished" : "Download Finished with Problems"
      body = item.displayBytes > 0 ? "\(name) · \(Format.bytes(item.displayBytes, locale: locale))" : name
    case .failed:
      title = "Download Failed"
      body = "\(name) · \(StatusText.line(for: item, locale: locale))"
    case .needsAttention:
      title = StatusText.headline(for: item) ?? "Needs Attention"
      body = "\(name) · \(StatusText.line(for: item, locale: locale))"
    case .queued, .running, .paused, .stopped:
      return nil
    }
  }
}

extension NotificationText {
  /// Where a notification keeps the id of the item it is about.
  static let itemKey = "itemID"
  /// Where the Mac app kept it before, so a notification delivered before
  /// an update still leads to its item.
  static let earlierItemKey = "item"

  /// A notification in these words ("Download Finished", "Sintel · 7.6 GB"),
  /// threaded with the others and tagged with the item for a click or tap
  /// (`itemID(from:)`). Nil when there is nothing to say. Each app adds its
  /// own actions.
  public static func content(for item: DownloadItem, locale: Locale = .autoupdatingCurrent) -> UNMutableNotificationContent? {
    guard let text = NotificationText(item, locale: locale) else { return nil }
    let content = UNMutableNotificationContent()
    content.title = text.title
    content.body = text.body
    content.threadIdentifier = "downloads"
    content.userInfo = [itemKey: item.id.uuidString]
    return content
  }

  /// The item a notification from `content(for:)` is about.
  public static func itemID(from userInfo: [AnyHashable: Any]) -> DownloadItem.ID? {
    let raw = userInfo[itemKey] ?? userInfo[earlierItemKey]
    return (raw as? String).flatMap(UUID.init(uuidString:))
  }
}
