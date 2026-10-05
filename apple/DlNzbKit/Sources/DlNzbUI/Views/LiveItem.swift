import DlNzbKit
import SwiftUI

/// Shows a download as it is now, progress included.
///
/// Progress changes a running item without changing `DownloadQueue.items`
/// (see `DownloadQueue.live`), so a list redraws only when its items change
/// state. A row or detail view that shows the numbers
/// reads its item through this: it redraws with them, and the list around
/// it does not.
public struct LiveItem<Content: View>: View {
  let item: DownloadItem
  let queue: DownloadQueue
  let content: (DownloadItem) -> Content

  public init(_ item: DownloadItem, in queue: DownloadQueue, @ViewBuilder content: @escaping (DownloadItem) -> Content) {
    self.item = item
    self.queue = queue
    self.content = content
  }

  public var body: some View {
    content(queue.live(item))
  }
}

extension DownloadItem {
  /// The same download apart from its progress. A view inside `LiveItem`
  /// that shows none of the numbers (the files, the folder, the details)
  /// compares its item this way, so it is not drawn again with every
  /// progress update.
  public func equalsIgnoringProgress(_ other: DownloadItem) -> Bool {
    var this = self
    var other = other
    this.progress = nil
    other.progress = nil
    return this == other
  }
}
