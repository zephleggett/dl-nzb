import AppKit
import DlNzbKit
import Foundation
import Testing

@testable import DlNzbApp

/// Records what the delegate hands over.
@MainActor
final class FakeOpenTarget: OpenTarget {
  private(set) var opened: [[URL]] = []
  private(set) var windowShown = 0

  func open(_ urls: [URL]) {
    opened.append(urls)
  }

  func showMainWindow() {
    windowShown += 1
  }
}

@Suite("Opening files from Finder and the Dock")
@MainActor
struct AppDelegateTests {
  let first = URL(filePath: "/tmp/First.nzb")
  let second = URL(filePath: "/tmp/Second.nzb")

  @Test("Files opened before the window exists wait, in order, and arrive once it does")
  func buffersUntilTheWindowExists() {
    let delegate = AppDelegate()
    delegate.application(NSApplication.shared, open: [first])
    delegate.application(NSApplication.shared, open: [second, first])
    #expect(delegate.buffer.pending == [first, second])

    let target = FakeOpenTarget()
    delegate.target = target
    #expect(target.opened == [[first, second]])
    #expect(target.windowShown == 1)
    #expect(delegate.buffer.isEmpty)
  }

  @Test("Once the window exists, files go straight through and bring it forward")
  func deliversDirectly() {
    let delegate = AppDelegate()
    let target = FakeOpenTarget()
    delegate.target = target
    #expect(target.opened.isEmpty)
    #expect(target.windowShown == 0)

    delegate.application(NSApplication.shared, open: [second])
    #expect(target.opened == [[second]])
    #expect(target.windowShown == 1)
  }

  @Test("A second window coming up does not deliver the same files again")
  func deliversOnce() {
    let delegate = AppDelegate()
    delegate.application(NSApplication.shared, open: [first])
    let target = FakeOpenTarget()
    delegate.target = target
    delegate.target = target
    #expect(target.opened == [[first]])
  }

  @Test("Clicking the Dock icon with no window open shows it again")
  func reopenShowsTheWindow() {
    let delegate = AppDelegate()
    let target = FakeOpenTarget()
    delegate.target = target
    #expect(delegate.applicationShouldHandleReopen(NSApplication.shared, hasVisibleWindows: true))
    #expect(target.windowShown == 0)
    #expect(delegate.applicationShouldHandleReopen(NSApplication.shared, hasVisibleWindows: false))
    #expect(target.windowShown == 1)
  }

  @Test("Closing the last window keeps downloads running")
  func closingTheWindowDoesNotQuit() {
    #expect(!AppDelegate().applicationShouldTerminateAfterLastWindowClosed(NSApplication.shared))
  }

  @Test("Without an app, quitting is immediate")
  func quitsWithoutAnApp() {
    #expect(AppDelegate().applicationShouldTerminate(NSApplication.shared) == .terminateNow)
  }
}

@Suite("Open request buffer")
struct OpenRequestBufferTests {
  @Test("Keeps each file once, in the order first seen, and empties when drained")
  func keepsOrderAndUniqueness() {
    var buffer = OpenRequestBuffer()
    let a = URL(filePath: "/a.nzb")
    let b = URL(filePath: "/b.nzb")
    buffer.add([a, b, a])
    buffer.add([b])
    #expect(buffer.drain() == [a, b])
    #expect(buffer.isEmpty)
    #expect(buffer.drain().isEmpty)
  }
}

@Suite("Selection after removing")
@MainActor
struct SelectionAfterRemovingTests {
  static func app(_ items: [DownloadItem]) -> MacApp {
    MacApp(model: .preview(items: items), services: nil)
  }

  @Test("The row after the removed one is selected, or the one before at the end of the list")
  func movesToNeighbour() {
    let items = [PreviewData.queued, PreviewData.downloading, PreviewData.paused]
    let app = Self.app(items)
    #expect(app.selectionAfterRemoving([items[0].id]) == [items[1].id])
    #expect(app.selectionAfterRemoving([items[2].id]) == [items[1].id])
    #expect(app.selectionAfterRemoving([items[0].id, items[1].id]) == [items[2].id])
    #expect(app.selectionAfterRemoving(Set(items.map(\.id))).isEmpty)
  }
}

@Suite("Dock menu")
@MainActor
struct DockMenuTests {
  @Test("The Dock menu offers Pause All while downloads run, and Resume All once paused")
  func pauseOrResume() throws {
    let delegate = AppDelegate()
    delegate.app = MacApp(model: .preview(items: [PreviewData.downloading]), services: nil)
    let running = try #require(delegate.applicationDockMenu(NSApplication.shared))
    #expect(running.items.map(\.title) == ["Pause All"])
    #expect(running.items.first?.isEnabled == true)

    delegate.app = MacApp(model: .preview(items: [PreviewData.paused]), services: nil)
    let paused = try #require(delegate.applicationDockMenu(NSApplication.shared))
    #expect(paused.items.map(\.title) == ["Resume All"])
  }

  @Test("With nothing in the list, the item is there but off")
  func empty() throws {
    let delegate = AppDelegate()
    delegate.app = MacApp(model: .preview(items: []), services: nil)
    let menu = try #require(delegate.applicationDockMenu(NSApplication.shared))
    #expect(menu.items.first?.isEnabled == false)
  }

  @Test("Once everything has finished, Pause All is off")
  func allFinished() throws {
    let delegate = AppDelegate()
    let app = MacApp(model: .preview(items: [PreviewData.finished, PreviewData.failed]), services: nil)
    delegate.app = app
    let menu = try #require(delegate.applicationDockMenu(NSApplication.shared))
    #expect(menu.items.map(\.title) == ["Pause All"])
    #expect(menu.items.first?.isEnabled == false)
    #expect(!app.canToggleAll)
  }
}
