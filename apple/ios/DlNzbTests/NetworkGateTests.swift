import DlNzbKit
import Foundation
import Testing

@testable import DlNzbApp

@MainActor
@Suite("Cellular gate")
struct NetworkGateTests {
  let queue = FakeQueue([Items.make(1, .queued), Items.make(2, .paused)])
  let holds: QueueHolds
  let gate: NetworkGate
  private let allows: Flag
  private let ran: Counter

  init() {
    holds = QueueHolds(queue: queue, defaults: scratchDefaults())
    let allows = Flag()
    let ran = Counter()
    self.allows = allows
    self.ran = ran
    gate = NetworkGate(queue: queue, holds: holds, allowsCellular: { allows.value })
    gate.onActionsRan = { ran.count += 1 }
  }

  @Test("On an ordinary network, actions run at once")
  func ordinaryNetwork() {
    var didRun = false
    gate.perform { didRun = true }
    #expect(didRun)
    #expect(!gate.isAsking)
    #expect(ran.count == 1)
  }

  @Test("On cellular, an action waits for an answer; Download runs it and lets the queue go")
  func asksThenAgrees() {
    gate.networkChanged(isExpensive: true)
    // Work is waiting to start, so the queue is held and the user is asked.
    #expect(holds.isHolding(.cellular))
    #expect(gate.isAsking)

    var didRun = false
    gate.perform { didRun = true }
    #expect(!didRun)

    gate.agree()
    #expect(didRun)
    #expect(!gate.isAsking)
    #expect(!holds.isHolding)
    #expect(!queue.isPaused)
    // Paused by the user before the hold: still paused.
    #expect(queue.state(Items.make(2, .queued).id) == .paused)

    // Agreed for this network: the next action runs at once.
    var again = false
    gate.perform { again = true }
    #expect(again)
  }

  @Test("Wait for Wi-Fi keeps everything waiting until the network is ordinary again")
  func waitsForWiFi() {
    gate.networkChanged(isExpensive: true)
    var didRun = false
    gate.perform { didRun = true }
    gate.wait()
    #expect(!gate.isAsking)
    #expect(!didRun)
    #expect(queue.isPaused)

    gate.networkChanged(isExpensive: false)
    #expect(didRun)
    #expect(!queue.isPaused)
    #expect(ran.count == 1)

    // Agreement does not carry over to the next expensive network.
    gate.networkChanged(isExpensive: true)
    #expect(gate.isAsking)
  }

  @Test("Allowing cellular in Settings never asks, and lets go of a hold")
  func allowedInSettings() {
    gate.networkChanged(isExpensive: true)
    #expect(holds.isHolding(.cellular))
    allows.value = true
    gate.reevaluate()
    #expect(!holds.isHolding)
    #expect(!gate.isAsking)

    var didRun = false
    gate.perform { didRun = true }
    #expect(didRun)
  }

  @Test("A download running when the network turns expensive pauses and asks")
  func runningDownloadPauses() {
    queue.items = [Items.downloading(3, done: 10, of: 100)]
    gate.networkChanged(isExpensive: true)
    #expect(queue.state(Items.make(3, .queued).id) == .paused)
    #expect(gate.isAsking)
    gate.agree()
    #expect(queue.state(Items.make(3, .queued).id) == .queued)
  }

  @Test("Adding on cellular holds first and asks only when something was added")
  func addingOnCellular() {
    queue.items = []
    gate.networkChanged(isExpensive: true)
    #expect(!gate.isAsking)

    let held = gate.prepareToAdd()
    #expect(held && queue.isPaused)
    gate.finishAdding(addedAny: false, heldForAdding: held)
    #expect(!holds.isHolding)
    #expect(!gate.isAsking)

    let heldAgain = gate.prepareToAdd()
    queue.items = [Items.make(4, .queued)]
    gate.finishAdding(addedAny: true, heldForAdding: heldAgain)
    #expect(holds.isHolding(.cellular))
    #expect(gate.isAsking)
  }
}

@MainActor
final class Flag {
  var value = false
}

@MainActor
final class Counter {
  var count = 0
}

@MainActor
@Suite("Cellular gate after a relaunch")
struct NetworkGateRelaunchTests {
  @Test("A cellular hold saved before a relaunch lets go on the first ordinary reading")
  func staleHoldReleased() {
    let defaults = scratchDefaults()
    let queue = FakeQueue([Items.make(1, .queued)])
    QueueHolds(queue: queue, defaults: defaults).hold(.cellular)
    #expect(queue.isPaused)

    let holds = QueueHolds(queue: queue, defaults: defaults)
    let gate = NetworkGate(queue: queue, holds: holds, allowsCellular: { false })
    gate.networkChanged(isExpensive: false)
    #expect(!holds.isHolding)
    #expect(!queue.isPaused)
  }
}
