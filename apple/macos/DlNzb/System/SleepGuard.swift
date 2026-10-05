import DlNzbKit
import Foundation

/// Holds a user-initiated activity while any job runs.
///
/// Always at least `.userInitiatedAllowingIdleSystemSleep`: App Nap would
/// otherwise throttle the in-process engine as soon as the window is hidden
/// (jetlink learned this the hard way). With Prevent sleep while downloading
/// on, plain `.userInitiated`, which also holds off idle sleep. Display sleep
/// is never prevented.
@MainActor
final class SleepGuard {
  enum Mode: Equatable {
    case preventIdleSleep
    case allowIdleSleep

    var options: ProcessInfo.ActivityOptions {
      switch self {
      case .preventIdleSleep: .userInitiated
      case .allowIdleSleep: .userInitiatedAllowingIdleSystemSleep
      }
    }
  }

  /// The activity to hold, or nil for none.
  nonisolated static func mode(active: Bool, preventSleep: Bool) -> Mode? {
    guard active else { return nil }
    return preventSleep ? .preventIdleSleep : .allowIdleSleep
  }

  private(set) var mode: Mode?
  private var activity: (any NSObjectProtocol)?

  func update(active: Bool, preventSleep: Bool) {
    let next = Self.mode(active: active, preventSleep: preventSleep)
    guard next != mode else { return }
    end()
    if let next {
      activity = ProcessInfo.processInfo.beginActivity(options: next.options, reason: "Downloading")
      Log.mac.info("holding an activity: \(next == .preventIdleSleep ? "idle sleep prevented" : "App Nap prevented", privacy: .public)")
    }
    mode = next
  }

  func end() {
    if let activity {
      ProcessInfo.processInfo.endActivity(activity)
      Log.mac.info("released the activity")
    }
    activity = nil
    mode = nil
  }
}
