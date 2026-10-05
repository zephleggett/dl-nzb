import DlNzbKit
import Foundation
import Testing

@testable import DlNzbApp

@MainActor
@Suite("Queue holds")
struct QueueHoldsTests {
  @Test("The queue resumes only when the last hold lets go, and only what the holds paused")
  func lastReleaseResumes() {
    let queue = FakeQueue([Items.downloading(1, done: 1, of: 10), Items.make(2, .paused), Items.make(3, .queued)])
    let holds = QueueHolds(queue: queue, defaults: scratchDefaults())
    holds.hold(.cellular)
    holds.hold(.backgroundExpired)
    #expect(queue.calls.filter { $0 == "pauseAll" }.count == 1)

    holds.release(.cellular)
    #expect(queue.isPaused)
    holds.release(.backgroundExpired)
    #expect(!queue.isPaused)
    #expect(queue.state(Items.make(1, .queued).id) == .queued)
    #expect(queue.state(Items.make(2, .queued).id) == .paused)
  }

  @Test("Pause All pressed during a hold, or before it, is still on afterwards")
  func userPauseAllWins() {
    let queue = FakeQueue([Items.downloading(1, done: 1, of: 10)])
    let holds = QueueHolds(queue: queue, defaults: scratchDefaults())
    holds.hold(.cellular)
    holds.userPausedAll()
    holds.release(.cellular)
    #expect(queue.isPaused)
    #expect(queue.state(Items.make(1, .queued).id) == .paused)

    let before = FakeQueue([Items.downloading(4, done: 1, of: 10), Items.make(5, .queued)])
    before.isPaused = true
    let other = QueueHolds(queue: before, defaults: scratchDefaults())
    other.hold(.backgroundExpired)
    other.release(.backgroundExpired)
    // The download the hold paused goes on; Pause All stays.
    #expect(before.isPaused)
    #expect(before.state(Items.make(4, .queued).id) == .queued)
  }

  @Test("An item the user pauses during a hold stays paused")
  func userPausedItem() {
    let queue = FakeQueue([Items.make(1, .queued), Items.make(2, .queued)])
    let holds = QueueHolds(queue: queue, defaults: scratchDefaults())
    holds.hold(.cellular)
    queue.pause(Items.make(2, .queued).id)
    holds.userPaused(Items.make(2, .queued).id)
    holds.release(.cellular)
    #expect(queue.state(Items.make(2, .queued).id) == .paused)
  }

  @Test("Holds survive a relaunch, so the pause is still known to be the app's own")
  func persists() {
    let defaults = scratchDefaults()
    let queue = FakeQueue([Items.downloading(1, done: 1, of: 10)])
    QueueHolds(queue: queue, defaults: defaults).hold(.backgroundExpired)

    let relaunched = QueueHolds(queue: queue, defaults: defaults)
    #expect(relaunched.isHolding(.backgroundExpired))
    relaunched.release(.backgroundExpired)
    #expect(!queue.isPaused)
    #expect(defaults.data(forKey: QueueHolds.defaultsKey) == nil)
  }
}
