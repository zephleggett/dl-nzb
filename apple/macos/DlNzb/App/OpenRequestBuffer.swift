import Foundation

/// Something that can take NZBs opened from Finder or the Dock and bring the
/// main window forward. `MacApp` in the app; a fake in the tests.
@MainActor
protocol OpenTarget: AnyObject {
  func open(_ urls: [URL])
  func showMainWindow()
}

/// Files opened before the app can show them.
///
/// A double-click in Finder launches dl-nzb and delivers the file through
/// `application(_:open:)`, which can arrive before SwiftUI has built the
/// window that would show a duplicate's alert. The delegate keeps them here
/// until the window is there, in the order they came, each file once.
struct OpenRequestBuffer: Equatable {
  private(set) var pending: [URL] = []

  var isEmpty: Bool { pending.isEmpty }

  mutating func add(_ urls: [URL]) {
    for url in urls where !pending.contains(url) {
      pending.append(url)
    }
  }

  /// Everything waiting, oldest first, leaving the buffer empty.
  mutating func drain() -> [URL] {
    defer { pending = [] }
    return pending
  }
}
