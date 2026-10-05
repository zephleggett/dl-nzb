import Foundation
import Testing

@testable import DlNzbApp

@Suite("Dock tile")
struct DockTileMachineTests {
  @Test("The custom tile goes in when a download starts and comes out when nothing runs")
  func installsAndRestores() {
    var machine = DockTileMachine()
    #expect(machine.update(fraction: nil, badgeCount: 0).isEmpty)

    let begin = machine.update(fraction: 0.1, badgeCount: 2)
    #expect(begin == [.install, .redraw(0.1), .badge("2")])
    #expect(machine.isShowing)

    let end = machine.update(fraction: nil, badgeCount: 0)
    #expect(end == [.restore, .badge(nil)])
    #expect(!machine.isShowing)
  }

  @Test("A change too small to see costs no redraw; one that shows is drawn at once")
  func ignoresInvisibleChanges() {
    var machine = DockTileMachine()
    _ = machine.update(fraction: 0.5, badgeCount: 1)
    #expect(machine.update(fraction: 0.501, badgeCount: 1).isEmpty)
    #expect(machine.update(fraction: 0.6, badgeCount: 1) == [.redraw(0.6)])
  }

  @Test("The badge follows the count of unfinished downloads and goes with the tile")
  func badge() {
    var machine = DockTileMachine()
    _ = machine.update(fraction: 0, badgeCount: 3)
    #expect(machine.update(fraction: 0, badgeCount: 2) == [.badge("2")])
    #expect(machine.update(fraction: 0, badgeCount: 0) == [.badge(nil)])
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
