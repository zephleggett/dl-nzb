import DlNzbKit
import DlNzbUI
import Foundation

/// How far a queue run has got, in bytes, for the continued-processing task.
///
/// Each item weighs its size. Moving bytes fills the first 95% of its share,
/// so the system's bar follows the "3.1 of 8.2 GB" beside it; verifying,
/// repairing and extracting fill the last 5%, so a long repair still moves
/// the bar and the system never takes the run for stalled. An item that is
/// over (finished, failed, needing attention) counts in full: it is done as
/// far as this run goes.
struct RunProgress: Equatable, Sendable {
  var completed: Int64
  var total: Int64

  static let downloadShare = 0.95

  static func of(_ items: [DownloadItem]) -> RunProgress {
    var completed: Double = 0
    var total: Int64 = 0
    for item in items {
      let weight = max(item.totalBytes, 1)
      total += weight
      completed += Double(weight) * share(of: item)
    }
    return RunProgress(completed: min(Int64(completed.rounded(.down)), total), total: total)
  }

  /// 0...1 of an item's weight.
  static func share(of item: DownloadItem) -> Double {
    switch item.state {
    case .queued, .paused:
      return downloadShare * item.downloadFraction
    case .running(let phase):
      if phase.usesNetwork { return downloadShare * item.downloadFraction }
      return downloadShare + (1 - downloadShare) * processingFraction(phase, item.phaseProgress?.displayFraction ?? 0)
    case .finished, .failed, .stopped, .needsAttention:
      return 1
    }
  }

  /// Post-processing as one stretch: verifying, then repairing, then
  /// extracting and renaming, each a part of it.
  private static func processingFraction(_ phase: JobPhase, _ fraction: Double) -> Double {
    switch phase {
    case .verifying: 0.3 * fraction
    case .repairing: 0.3 + 0.3 * fraction
    case .extracting: 0.6 + 0.35 * fraction
    case .renaming: 0.95 + 0.05 * fraction
    case .connecting, .checking, .downloading, .downloadingRecovery: 0
    }
  }
}

/// The title and subtitle of the system's Live Activity for a queue run:
/// the release in hand, and how far it has got with what is still to come.
enum ContinuedTaskText {
  /// The release's name, spaced as words ("Sintel 2010 2160p"): the system
  /// truncates a long title and VoiceOver reads it aloud.
  static func title(for item: DownloadItem?) -> String {
    guard let item else { return "Downloading" }
    return ReleaseText.spoken(item.displayTitle)
  }

  /// "3.1 of 8.2 GB · 2 waiting" while bytes move; otherwise the row's own
  /// status line ("Extracting · 2 of 5"), and the waiting count after it.
  static func subtitle(for item: DownloadItem?, waiting: Int = 0, locale: Locale = .autoupdatingCurrent) -> String {
    guard let item else { return "Starting" }
    let status: String
    if let amount = RowStatus.transferAmount(for: item, locale: locale) {
      status = amount
    } else if item.phase == .downloading, item.totalBytes > 0 {
      status = compactBytes(0, of: item.totalBytes, locale: locale)
    } else {
      status = StatusText.line(for: item, locale: locale)
    }
    guard waiting > 0 else { return status }
    return "\(status) · \(Format.count(waiting, locale: locale)) waiting"
  }

  /// "3.1 of 8.2 GB", or "512 MB of 8.2 GB" when the units differ.
  static func compactBytes(_ done: Int64, of total: Int64, locale: Locale = .autoupdatingCurrent) -> String {
    let done = Format.bytes(done, locale: locale)
    let total = Format.bytes(total, locale: locale)
    guard let doneSplit = done.lastIndex(where: \.isWhitespace), let totalSplit = total.lastIndex(where: \.isWhitespace),
      done[done.index(after: doneSplit)...] == total[total.index(after: totalSplit)...]
    else { return "\(done) of \(total)" }
    return "\(done[..<doneSplit]) of \(total)"
  }
}
