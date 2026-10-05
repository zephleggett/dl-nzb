import BackgroundTasks
import Foundation
import UIKit

/// A request for one `BGContinuedProcessingTask`.
struct ContinuedTaskRequest: Equatable, Sendable {
  var identifier: String
  var title: String
  var subtitle: String
}

/// The system's continued-processing scheduler as `BackgroundRunner` uses it,
/// so the tests can stand in for it. Launch handlers run on the main actor.
@MainActor
protocol ContinuedTaskScheduling: AnyObject {
  /// Registers the handler for exactly this identifier. False when the
  /// identifier is not permitted by the Info.plist.
  func register(_ identifier: String, launchHandler: @escaping @MainActor (any ContinuedTask) -> Void) -> Bool
  func submit(_ request: ContinuedTaskRequest) throws
  /// Withdraws a request the system has not started.
  func cancel(_ identifier: String)
}

/// A running continued-processing task: its progress (shown by the system's
/// Live Activity), its title and subtitle, expiry and completion.
@MainActor
protocol ContinuedTask: AnyObject {
  var progress: Progress { get }
  func update(title: String, subtitle: String)
  /// Called on the main actor when the system or the user ends the task early.
  func setExpirationHandler(_ handler: @escaping @MainActor () -> Void)
  func complete(success: Bool)
}

/// Background time while the app leaves the screen, when no continued task
/// covers it: `UIApplication.beginBackgroundTask`.
@MainActor
protocol BackgroundTimeProviding: AnyObject {
  /// Nil when the system gives no time at all.
  func begin(name: String, expiration: @escaping @MainActor () -> Void) -> Int?
  func end(_ token: Int)
}

// MARK: The system's

/// `BGTaskScheduler`, for continued processing only.
@MainActor
final class SystemContinuedTaskScheduler: ContinuedTaskScheduling {
  func register(_ identifier: String, launchHandler: @escaping @MainActor (any ContinuedTask) -> Void) -> Bool {
    // The main queue, so the handler is on the main actor as it runs.
    BGTaskScheduler.shared.register(forTaskWithIdentifier: identifier, using: .main) { task in
      MainActor.assumeIsolated {
        guard let continued = task as? BGContinuedProcessingTask else {
          task.setTaskCompleted(success: false)
          return
        }
        launchHandler(SystemContinuedTask(continued))
      }
    }
  }

  func submit(_ request: ContinuedTaskRequest) throws {
    let task = BGContinuedProcessingTaskRequest(identifier: request.identifier, title: request.title, subtitle: request.subtitle)
    // Queue rather than fail: the run is worth covering whenever the system
    // has room for it, and the downloads start in the foreground regardless.
    // A queued request that starts after the run is over completes at once.
    task.strategy = .queue
    try BGTaskScheduler.shared.submit(task)
  }

  func cancel(_ identifier: String) {
    BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: identifier)
  }
}

/// A `BGContinuedProcessingTask`. Its methods are safe from any thread; it is
/// only touched from the main actor here, apart from the expiration handler,
/// which hops there.
@MainActor
final class SystemContinuedTask: ContinuedTask {
  private let task: BGContinuedProcessingTask

  init(_ task: BGContinuedProcessingTask) {
    self.task = task
  }

  var progress: Progress { task.progress }

  func update(title: String, subtitle: String) {
    task.updateTitle(title, subtitle: subtitle)
  }

  func setExpirationHandler(_ handler: @escaping @MainActor () -> Void) {
    task.expirationHandler = {
      // The system calls this on a queue of its choosing and wants it brief:
      // hop to the main actor, where the queue lives, and return.
      if Thread.isMainThread {
        MainActor.assumeIsolated { handler() }
      } else {
        DispatchQueue.main.async { handler() }
      }
    }
  }

  func complete(success: Bool) {
    task.setTaskCompleted(success: success)
  }
}

/// `UIApplication.beginBackgroundTask`, whose expiration handler UIKit calls
/// on the main thread.
@MainActor
final class SystemBackgroundTime: BackgroundTimeProviding {
  func begin(name: String, expiration: @escaping @MainActor () -> Void) -> Int? {
    let identifier = UIApplication.shared.beginBackgroundTask(withName: name) {
      MainActor.assumeIsolated { expiration() }
    }
    return identifier == .invalid ? nil : identifier.rawValue
  }

  func end(_ token: Int) {
    UIApplication.shared.endBackgroundTask(UIBackgroundTaskIdentifier(rawValue: token))
  }
}
