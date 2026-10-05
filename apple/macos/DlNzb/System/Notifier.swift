import AppKit
import DlNzbKit
import DlNzbUI
import UserNotifications

/// What one notification says (the Kit's `NotificationText`), and whether
/// it offers Show in Finder, as finished downloads do.
struct FinishNotification: Equatable {
  let title: String
  let body: String
  let offersReveal: Bool

  init?(_ item: DownloadItem) {
    guard let text = NotificationText(item) else { return nil }
    title = text.title
    body = text.body
    offersReveal = item.isFinished
  }
}

/// One notification per job that finishes, fails or needs attention, through
/// `UNUserNotificationCenter`. Permission is asked for the first time there is
/// something to say, not at launch. Banners only show while dl-nzb is in the
/// background; in front, the row already says it, so they go straight to
/// Notification Center.
@MainActor
final class Notifier: NSObject, UNUserNotificationCenterDelegate {
  static let finishedCategory = "finished"
  static let revealAction = "reveal"

  /// Show in Finder on a notification.
  var onReveal: (@MainActor (DownloadItem.ID) -> Void)?
  /// A click on the notification itself.
  var onOpen: (@MainActor (DownloadItem.ID) -> Void)?

  private let center = UNUserNotificationCenter.current()
  /// The permission question while it is on screen: a second job finishing
  /// meanwhile waits for the same answer instead of asking again, which the
  /// system turns down with an error.
  private var pendingAuthorisation: Task<Bool, Never>?

  /// Early in launch, so a click that launched the app is delivered.
  func install() {
    center.delegate = self
    let reveal = UNNotificationAction(identifier: Self.revealAction, title: "Show in Finder", options: [.foreground])
    center.setNotificationCategories([UNNotificationCategory(identifier: Self.finishedCategory, actions: [reveal], intentIdentifiers: [])])
  }

  func notify(_ item: DownloadItem) {
    guard let message = FinishNotification(item) else { return }
    let content = UNMutableNotificationContent()
    content.title = message.title
    content.body = message.body
    content.threadIdentifier = "downloads"
    content.userInfo = ["item": item.id.uuidString]
    if message.offersReveal { content.categoryIdentifier = Self.finishedCategory }
    let request = UNNotificationRequest(identifier: item.id.uuidString, content: content, trigger: nil)
    Task {
      guard await authorise() else { return }
      do {
        try await center.add(request)
      } catch {
        Log.mac.error("the notification was not shown: \(error.localizedDescription, privacy: .public)")
      }
    }
  }

  /// Asks the system each time rather than remembering an answer: the user
  /// can allow notifications in System Settings, or answer the question late,
  /// while dl-nzb keeps running.
  private func authorise() async -> Bool {
    switch await center.notificationSettings().authorizationStatus {
    case .authorized, .provisional, .ephemeral: return true
    case .denied: return false
    case .notDetermined: break
    @unknown default: return false
    }
    if let pendingAuthorisation { return await pendingAuthorisation.value }
    let center = center
    let request = Task<Bool, Never> {
      do {
        // No sounds. The badge is the Dock's count of unfinished downloads,
        // which macOS shows only for an app allowed to badge.
        return try await center.requestAuthorization(options: [.alert, .badge])
      } catch {
        Log.mac.error("notification permission failed: \(error.localizedDescription, privacy: .public)")
        return false
      }
    }
    pendingAuthorisation = request
    let granted = await request.value
    pendingAuthorisation = nil
    return granted
  }

  // MARK: UNUserNotificationCenterDelegate

  nonisolated func userNotificationCenter(
    _ center: UNUserNotificationCenter, willPresent notification: UNNotification
  ) async -> UNNotificationPresentationOptions {
    let active = await MainActor.run { NSApp.isActive }
    return active ? [.list] : [.banner, .list]
  }

  nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
    let action = response.actionIdentifier
    guard let text = response.notification.request.content.userInfo["item"] as? String, let id = UUID(uuidString: text) else { return }
    await MainActor.run {
      if action == Self.revealAction {
        onReveal?(id)
      } else if action == UNNotificationDefaultActionIdentifier {
        onOpen?(id)
      }
    }
  }
}
