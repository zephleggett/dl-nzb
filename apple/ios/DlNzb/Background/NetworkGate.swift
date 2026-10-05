import DlNzbKit
import Foundation
import Network
import Observation

/// Asks before downloading on cellular, or on any network the system calls
/// expensive or constrained (a personal hotspot, Low Data Mode), unless the
/// user allowed cellular in Settings.
///
/// While such a network is in use and the user has not agreed this time:
/// - starting something (adding an NZB, Resume, Retry, Start) does not run it
///   but asks "Download on Cellular?", and the action waits;
/// - downloads already running when the network changes pause (through
///   `QueueHolds`) and the same question is asked.
///
/// Download agrees until the next time the device is back on an ordinary
/// network. Wait for Wi-Fi keeps everything waiting; it all goes ahead by
/// itself once the network is ordinary again.
@MainActor
@Observable
final class NetworkGate {
  /// Whether the alert should be up.
  var isAsking = false
  private(set) var isExpensive = false
  /// The user agreed to this expensive network until the next ordinary one.
  private(set) var isAgreed = false
  @ObservationIgnored private var hasReading = false

  @ObservationIgnored private let queue: any QueueControlling
  @ObservationIgnored private let holds: QueueHolds
  @ObservationIgnored private let allowsCellular: @MainActor () -> Bool
  /// Actions that asked and are waiting for an answer or for Wi-Fi.
  @ObservationIgnored private var waiting: [@MainActor () -> Void] = []
  /// Runs after waiting actions go ahead: the background runner's cue.
  @ObservationIgnored var onActionsRan: (@MainActor () -> Void)?

  init(queue: any QueueControlling, holds: QueueHolds, allowsCellular: @escaping @MainActor () -> Bool) {
    self.queue = queue
    self.holds = holds
    self.allowsCellular = allowsCellular
  }

  /// Downloads would go over an expensive network the user has not agreed to.
  var isBlocking: Bool {
    isExpensive && !allowsCellular() && !isAgreed
  }

  // MARK: Inputs

  /// The path changed: from `PathMonitor`, or a test.
  func networkChanged(isExpensive: Bool) {
    // The first reading always counts: a hold saved before a relaunch is
    // let go here when the network turns out to be ordinary.
    guard !hasReading || isExpensive != self.isExpensive else { return }
    hasReading = true
    self.isExpensive = isExpensive
    AppLog.network.info("the network is \(isExpensive ? "expensive" : "ordinary", privacy: .public)")
    if !isExpensive { isAgreed = false }
    reevaluate()
  }

  /// The setting changed, or the queue may have new work: hold or let go.
  func reevaluate() {
    if isBlocking {
      if queue.hasNetworkWork || !waiting.isEmpty {
        holds.hold(.cellular)
        isAsking = true
      }
    } else {
      isAsking = false
      if holds.isHolding(.cellular) { holds.release(.cellular) }
      runWaiting()
    }
  }

  /// Runs `action` now, or asks first and runs it once the user agrees or
  /// the network is ordinary again.
  func perform(_ action: @escaping @MainActor () -> Void) {
    guard isBlocking else {
      action()
      onActionsRan?()
      return
    }
    waiting.append(action)
    holds.hold(.cellular)
    isAsking = true
  }

  /// Before adding NZBs: holds the queue, so a new item does not start by
  /// itself before the user answers. True when this call put the hold on.
  func prepareToAdd() -> Bool {
    guard isBlocking, !holds.isHolding(.cellular) else { return false }
    holds.hold(.cellular)
    return true
  }

  /// After adding: asks when something new is waiting for the network, and
  /// lets go of a hold `prepareToAdd` put on for nothing.
  func finishAdding(addedAny: Bool, heldForAdding: Bool) {
    guard isBlocking else { return }
    if addedAny {
      isAsking = true
    } else if heldForAdding && waiting.isEmpty {
      holds.release(.cellular)
    }
  }

  // MARK: Answers

  /// Download: agreed for this expensive network.
  func agree() {
    AppLog.network.info("the user agreed to download on an expensive network")
    isAgreed = true
    reevaluate()
  }

  /// Wait for Wi-Fi: everything stays where it is until the network is ordinary.
  func wait() {
    AppLog.network.info("waiting for an ordinary network")
    isAsking = false
  }

  private func runWaiting() {
    guard !waiting.isEmpty else { return }
    let actions = waiting
    waiting = []
    for action in actions { action() }
    onActionsRan?()
  }
}

/// Watches the network path and tells the gate when it turns expensive or
/// constrained, or back.
@MainActor
final class PathMonitor {
  private let monitor = NWPathMonitor()

  func start(_ onChange: @escaping @MainActor (_ isExpensive: Bool) -> Void) {
    // On the main queue: each change reaches the gate in order, with no hop.
    monitor.pathUpdateHandler = { path in
      let expensive = path.status == .satisfied && (path.isExpensive || path.isConstrained)
      MainActor.assumeIsolated { onChange(expensive) }
    }
    monitor.start(queue: .main)
  }

  deinit {
    monitor.cancel()
  }
}
