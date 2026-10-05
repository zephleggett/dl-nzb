import DlNzbKit
import DlNzbUI
import Foundation
@preconcurrency import UserNotifications

/// One notification per job that finishes, fails or needs the user, posted
/// only while the app is out of sight (in front, the row says it already) and
/// only when "Notify when downloads finish" is on. Silent: SPEC's "quiet by
/// default".
///
/// Permission is asked for the first time the user adds a download, when the
/// reason is plain, not at launch.
@MainActor
final class Notifier: NSObject {
  nonisolated static let itemKey = "itemID"

  private let center: UNUserNotificationCenter
  /// A tap on a notification: the app selects that download.
  var onOpenItem: (@MainActor (DownloadItem.ID) -> Void)?

  init(center: UNUserNotificationCenter = .current()) {
    self.center = center
    super.init()
    center.delegate = self
  }

  /// Asks once, the first time it matters.
  func requestAuthorizationIfNeeded() async {
    let settings = await center.notificationSettings()
    guard settings.authorizationStatus == .notDetermined else { return }
    do {
      let granted = try await center.requestAuthorization(options: [.alert])
      AppLog.notifications.info("notifications \(granted ? "allowed" : "declined", privacy: .public)")
    } catch {
      AppLog.notifications.error("could not ask for notifications: \(error.localizedDescription, privacy: .public)")
    }
  }

  func post(for item: DownloadItem) {
    guard let content = Self.content(for: item) else { return }
    let request = UNNotificationRequest(identifier: item.id.uuidString, content: content, trigger: nil)
    center.add(request) { error in
      if let error {
        AppLog.notifications.error("could not post a notification: \(error.localizedDescription, privacy: .public)")
      }
    }
  }

  /// The queue paused in the background: one notice, replaced rather than
  /// repeated, and withdrawn when the app comes back.
  func postPausedInBackground() {
    let request = UNNotificationRequest(identifier: Self.pausedIdentifier, content: Self.pausedContent(), trigger: nil)
    center.add(request) { error in
      if let error {
        AppLog.notifications.error("could not post the paused notice: \(error.localizedDescription, privacy: .public)")
      }
    }
  }

  /// Back in front: the list says how things stand.
  func withdrawPausedNotice() {
    center.removeDeliveredNotifications(withIdentifiers: [Self.pausedIdentifier])
    center.removePendingNotificationRequests(withIdentifiers: [Self.pausedIdentifier])
  }

  nonisolated static let pausedIdentifier = "downloads-paused"

  /// "Downloads Paused", "Open dl-nzb to continue."
  nonisolated static func pausedContent() -> UNMutableNotificationContent {
    let content = UNMutableNotificationContent()
    content.title = "Downloads Paused"
    content.body = "Open dl-nzb to continue."
    content.threadIdentifier = "downloads"
    return content
  }

  /// The Kit's words for the item ("Download Finished", "Sintel · 7.6 GB"),
  /// threaded together and tagged with the item for a tap.
  static func content(for item: DownloadItem) -> UNMutableNotificationContent? {
    guard let text = NotificationText(item) else { return nil }
    let content = UNMutableNotificationContent()
    content.title = text.title
    content.body = text.body
    content.threadIdentifier = "downloads"
    content.userInfo = [itemKey: item.id.uuidString]
    return content
  }
}

extension Notifier: UNUserNotificationCenterDelegate {
  /// In front, the list says it: nothing to show.
  nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification) async
    -> UNNotificationPresentationOptions
  {
    []
  }

  nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
    guard let raw = response.notification.request.content.userInfo[Self.itemKey] as? String, let id = UUID(uuidString: raw) else { return }
    await MainActor.run {
      onOpenItem?(id)
    }
  }
}
