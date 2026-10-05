import Foundation
import Testing

@testable import DlNzbApp

@Suite("Dock tile")
struct DockTileMachineTests {
  let start = ContinuousClock.now

  @Test("The custom tile goes in when a download starts and comes out when nothing runs")
  func installsAndRestores() {
    var machine = DockTileMachine()
    #expect(machine.update(fraction: nil, badgeCount: 0, now: start).isEmpty)

    let begin = machine.update(fraction: 0.1, badgeCount: 2, now: start)
    #expect(begin == [.install, .redraw(0.1), .badge("2")])
    #expect(machine.isShowing)

    let end = machine.update(fraction: nil, badgeCount: 0, now: start + .seconds(5))
    #expect(end == [.restore, .badge(nil)])
    #expect(!machine.isShowing)
  }

  @Test("Redraws are held to about twice a second, and the latest value is drawn when one comes due")
  func throttles() {
    var machine = DockTileMachine()
    _ = machine.update(fraction: 0.1, badgeCount: 1, now: start)

    // A quarter of a second later: too soon, so a redraw is scheduled.
    let soon = machine.update(fraction: 0.2, badgeCount: 1, now: start + .milliseconds(250))
    #expect(soon == [.scheduleRedraw(.milliseconds(250))])
    // More progress while it is pending schedules nothing more.
    #expect(machine.update(fraction: 0.3, badgeCount: 1, now: start + .milliseconds(400)).isEmpty)
    // When it comes due, the latest value is drawn.
    #expect(machine.redrawDue(now: start + .milliseconds(500)) == [.redraw(0.3)])
    #expect(machine.redrawDue(now: start + .milliseconds(600)).isEmpty)

    // Half a second after the last draw, a change draws at once.
    #expect(machine.update(fraction: 0.4, badgeCount: 1, now: start + .seconds(1)) == [.redraw(0.4)])
  }

  @Test("A change too small to see costs no redraw")
  func ignoresInvisibleChanges() {
    var machine = DockTileMachine()
    _ = machine.update(fraction: 0.5, badgeCount: 1, now: start)
    #expect(machine.update(fraction: 0.501, badgeCount: 1, now: start + .seconds(1)).isEmpty)
  }

  @Test("The badge follows the count of unfinished downloads and goes with the tile")
  func badge() {
    var machine = DockTileMachine()
    _ = machine.update(fraction: 0, badgeCount: 3, now: start)
    #expect(machine.update(fraction: 0, badgeCount: 2, now: start + .seconds(1)) == [.badge("2")])
    #expect(machine.update(fraction: 0, badgeCount: 0, now: start + .seconds(2)) == [.badge(nil)])
  }

  @Test("A redraw that comes due after the tile was restored does nothing")
  func lateRedrawAfterRestore() {
    var machine = DockTileMachine()
    _ = machine.update(fraction: 0.1, badgeCount: 1, now: start)
    _ = machine.update(fraction: 0.2, badgeCount: 1, now: start + .milliseconds(100))
    _ = machine.update(fraction: nil, badgeCount: 0, now: start + .milliseconds(200))
    #expect(machine.redrawDue(now: start + .milliseconds(500)).isEmpty)
  }

  @Test("Fractions are clamped and rounded to a two-hundredth")
  func quantises() {
    #expect(DockTileMachine.quantised(-1) == 0)
    #expect(DockTileMachine.quantised(2) == 1)
    #expect(DockTileMachine.quantised(.nan) == 0)
    #expect(DockTileMachine.quantised(0.123) == 0.125)
  }
}

@Suite("Sleep guard")
struct SleepGuardTests {
  @Test("Nothing is held while idle")
  func idle() {
    #expect(SleepGuard.mode(active: false, preventSleep: true) == nil)
    #expect(SleepGuard.mode(active: false, preventSleep: false) == nil)
  }

  @Test("While active, idle sleep is held off only when the setting asks; App Nap always is")
  func active() {
    #expect(SleepGuard.mode(active: true, preventSleep: true) == .preventIdleSleep)
    #expect(SleepGuard.mode(active: true, preventSleep: false) == .allowIdleSleep)
    #expect(SleepGuard.Mode.preventIdleSleep.options == .userInitiated)
    #expect(SleepGuard.Mode.allowIdleSleep.options == .userInitiatedAllowingIdleSystemSleep)
  }
}
