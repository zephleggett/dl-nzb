import DlNzbKit
import Foundation

/// A published `Progress` on each running job's folder, the way Safari
/// shows a download: Finder draws a bar on the folder's icon, and the Dock's
/// Downloads stack shows it too when the folder is in ~/Downloads.
///
/// A progress is published once the engine has created the folder (it does
/// so as the job starts) and withdrawn when the job stops running. Finder's
/// cancel button on the icon stops the job, keeping its data.
@MainActor
final class FinderProgress {
  /// Finder's cancel button.
  var onCancel: (@MainActor (DownloadItem.ID) -> Void)?

  private var published: [DownloadItem.ID: Progress] = [:]

  func update(items: [DownloadItem]) {
    var running: Set<DownloadItem.ID> = []
    for item in items where item.isRunning {
      running.insert(item.id)
      let progress = published[item.id] ?? publish(item)
      guard let progress else { continue }
      let total = max(item.totalBytes, 1)
      progress.totalUnitCount = total
      progress.completedUnitCount = Int64(item.downloadFraction * Double(total))
      let transfer = item.progress.flatMap { $0.phase.isTransfer ? $0 : nil }
      progress.throughput = transfer.map { Int($0.speedBytesPerSecond) }
      progress.estimatedTimeRemaining = transfer?.etaSeconds.map(TimeInterval.init)
    }
    for id in published.keys where !running.contains(id) {
      published.removeValue(forKey: id)?.unpublish()
    }
  }

  func unpublishAll() {
    for progress in published.values { progress.unpublish() }
    published.removeAll()
  }

  private func publish(_ item: DownloadItem) -> Progress? {
    let folder = item.outputDirectory
    guard FileManager.default.fileExists(atPath: folder.path(percentEncoded: false)) else { return nil }
    let progress = Progress(totalUnitCount: max(item.totalBytes, 1))
    progress.kind = .file
    progress.fileOperationKind = .downloading
    progress.fileURL = folder
    progress.isCancellable = true
    progress.isPausable = false
    let id = item.id
    progress.cancellationHandler = { [weak self] in
      Task { @MainActor in self?.onCancel?(id) }
    }
    progress.publish()
    published[id] = progress
    Log.mac.debug("published Finder progress for \(folder.lastPathComponent, privacy: .public)")
    return progress
  }
}
