import DlNzbKit
import Foundation
import Observation

/// What the iPhone's own machinery needs from the queue. `DownloadQueue`
/// conforms as it is; the tests drive a fake, so the background and cellular
/// state machines can be checked step by step without an engine.
@MainActor
protocol QueueControlling: AnyObject {
  var items: [DownloadItem] { get }
  var isPaused: Bool { get }
  /// Items running in any phase.
  var activeCount: Int { get }
  /// The item downloading now, or else the first one being processed.
  var currentItem: DownloadItem? { get }
  /// A job holds or is about to hold server connections: what the cellular
  /// prompt guards.
  var hasNetworkWork: Bool { get }
  func pause(_ id: DownloadItem.ID)
  func resume(_ id: DownloadItem.ID)
  func pauseAll()
  func resumeAll(keepingPaused kept: Set<DownloadItem.ID>)
  func saveNow()
}

extension DownloadQueue: QueueControlling {}

/// Pauses the whole queue on the iPhone's behalf and later undoes exactly
/// that. Two things hold it: an expensive network the user has not agreed to
/// download on, and the system ending the app's background time. Either can
/// come and go while the other holds, so the queue resumes only when the last
/// one lets go, and then only what the holds paused: anything the user had
/// paused one by one before stays paused, and a queue the user had paused
/// with Pause All stays paused.
///
/// The state is saved, because the queue remembers Pause All across a
/// relaunch and the app must still know the pause was its own.
@MainActor
@Observable
final class QueueHolds {
  enum Reason: String, Codable {
    case cellular
    case backgroundExpired
  }

  /// What the queue looked like before the first hold.
  private struct Snapshot: Codable {
    var pausedBefore: Set<UUID>
    var queueWasPaused: Bool
  }

  private struct Stored: Codable {
    var reasons: Set<Reason>
    var snapshot: Snapshot?
  }

  private(set) var reasons: Set<Reason> = []
  @ObservationIgnored private var snapshot: Snapshot?
  @ObservationIgnored private let queue: any QueueControlling
  @ObservationIgnored private let defaults: UserDefaults
  static let defaultsKey = "queueHolds"

  init(queue: any QueueControlling, defaults: UserDefaults = .standard) {
    self.queue = queue
    self.defaults = defaults
    if let data = defaults.data(forKey: Self.defaultsKey), let stored = try? JSONDecoder().decode(Stored.self, from: data) {
      reasons = stored.reasons
      snapshot = stored.snapshot
    }
  }

  func isHolding(_ reason: Reason) -> Bool {
    reasons.contains(reason)
  }

  var isHolding: Bool { !reasons.isEmpty }

  func hold(_ reason: Reason) {
    guard !reasons.contains(reason) else { return }
    if reasons.isEmpty {
      snapshot = Snapshot(pausedBefore: pausedNow, queueWasPaused: queue.isPaused)
      queue.pauseAll()
    }
    reasons.insert(reason)
    AppLog.background.info("holding the queue: \(reason.rawValue, privacy: .public)")
    persist()
  }

  func release(_ reason: Reason) {
    guard reasons.remove(reason) != nil else { return }
    AppLog.background.info("released the hold: \(reason.rawValue, privacy: .public)")
    defer { persist() }
    guard reasons.isEmpty, let snapshot else { return }
    self.snapshot = nil
    if snapshot.queueWasPaused {
      // Pause All was the user's: only what the hold paused goes back.
      for item in queue.items where item.isPaused && !snapshot.pausedBefore.contains(item.id) {
        queue.resume(item.id)
      }
    } else if queue.isPaused {
      queue.resumeAll(keepingPaused: snapshot.pausedBefore)
    }
  }

  /// The user paused an item while a hold was on: it stays paused when the
  /// hold lets go.
  func userPaused(_ id: DownloadItem.ID) {
    guard snapshot != nil else { return }
    snapshot?.pausedBefore.insert(id)
    persist()
  }

  /// The user pressed Pause All while a hold was on: the queue stays paused
  /// when the hold lets go.
  func userPausedAll() {
    guard snapshot != nil else { return }
    snapshot = Snapshot(pausedBefore: pausedNow, queueWasPaused: true)
    persist()
  }

  /// The user pressed Resume All (or resumed the queue some other way) while
  /// a hold was on: the user decides now, so the holds are forgotten.
  func userResumed() {
    guard !reasons.isEmpty else { return }
    reasons.removeAll()
    snapshot = nil
    persist()
  }

  private var pausedNow: Set<DownloadItem.ID> {
    Set(queue.items.filter(\.isPaused).map(\.id))
  }

  private func persist() {
    if reasons.isEmpty {
      defaults.removeObject(forKey: Self.defaultsKey)
    } else if let data = try? JSONEncoder().encode(Stored(reasons: reasons, snapshot: snapshot)) {
      defaults.set(data, forKey: Self.defaultsKey)
    }
  }
}
