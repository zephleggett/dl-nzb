import Foundation
import Observation

/// Runs `body` now and again whenever anything it read changes, at most once
/// per main-actor turn: a burst of progress events is one update.
///
/// Observation reports the first change and then forgets; the next run of
/// `body` reads (and so tracks) everything afresh.
@MainActor
final class Watcher {
  private let body: @MainActor () -> Void
  private var isStopped = false

  init(_ body: @escaping @MainActor () -> Void) {
    self.body = body
    run()
  }

  func stop() {
    isStopped = true
  }

  private func run() {
    guard !isStopped else { return }
    withObservationTracking {
      body()
    } onChange: { [weak self] in
      Task { @MainActor in self?.run() }
    }
  }
}
