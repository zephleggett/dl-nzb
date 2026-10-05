import DlNzbKit
import DlNzbUI
import Foundation
import SwiftUI

/// The status line as an iPhone row shows it: the Kit's line (the SPEC's
/// table), made to fit a phone's width where it runs long. Sizes share their
/// unit ("3.1 of 8.2 GB"), and the speed is left to the screen's subtitle,
/// which carries it for the whole queue; finished rows keep what happened.
/// The detail screen shows the full line.
enum RowStatus {
  /// `held`: the queue is holding the item back (`DownloadQueue.isHeld`), so
  /// a waiting row reads "Paused".
  static func line(for item: DownloadItem, held: Bool = false, locale: Locale = .autoupdatingCurrent) -> String {
    switch item.state {
    case .running(let phase) where phase.isTransfer:
      guard let amount = transferAmount(for: item, locale: locale) else { break }
      if phase == .downloading, let progress = item.progress {
        if progress.speedBytesPerSecond < 1, progress.bytesDone < progress.bytesTotal {
          // Nothing arriving: connecting again, or a slow server (as StatusText).
          return "\(amount) · Waiting for the server…"
        }
        if let eta = progress.etaSeconds { return "\(amount) · \(Format.timeLeft(eta, locale: locale))" }
      }
      return amount
    case .paused, .stopped:
      guard let progress = item.progress, progress.phase.isTransfer, progress.bytesTotal > 0, progress.bytesDone > 0 else { break }
      let word = item.isPaused ? "Paused" : "Stopped"
      return "\(word) · \(ContinuedTaskText.compactBytes(progress.bytesDone, of: progress.bytesTotal, locale: locale))"
    case .finished(let summary):
      var parts = [Format.bytes(item.displayBytes, locale: locale)]
      if summary.par2.repairedBlocks > 0 {
        parts.append("Repaired \(Format.count(summary.par2.repairedBlocks, "block", "blocks", locale: locale))")
      } else if summary.outcome == .completed {
        parts.append("Finished in \(Format.duration(summary.elapsedSeconds, locale: locale))")
      }
      if summary.outcome == .completedWithIssues {
        parts.append(
          summary.archivesFailed > 0
            ? Format.count(summary.archivesFailed, "archive could not be extracted", "archives could not be extracted", locale: locale)
            : "With problems")
      }
      return parts.joined(separator: " · ")
    default:
      break
    }
    return StatusText.line(for: item, held: held, locale: locale)
  }

  /// "3.1 of 8.2 GB", or "Recovery data · 3.1 of 8.2 GB", while the item's
  /// bytes move; nil until there is a size to count against.
  static func transferAmount(for item: DownloadItem, locale: Locale = .autoupdatingCurrent) -> String? {
    guard let progress = item.phaseProgress, progress.phase.isTransfer, progress.bytesTotal > 0 else { return nil }
    let amount = ContinuedTaskText.compactBytes(progress.bytesDone, of: progress.bytesTotal, locale: locale)
    return progress.phase == .downloadingRecovery ? "Recovery data · \(amount)" : amount
  }

  /// "Downloading", "Paused", "Finished", "Needs Attention" …: the detail
  /// screen's word for the state.
  static func title(for item: DownloadItem, awaitsStart: Bool = false, held: Bool = false) -> String {
    if let headline = StatusText.headline(for: item) { return headline }
    switch item.state {
    case .queued: return held ? "Paused" : (awaitsStart ? "Ready" : "Waiting")
    case .running(let phase): return phase.title
    case .paused: return "Paused"
    case .stopped: return "Stopped"
    case .finished(let summary): return summary.outcome == .completed ? "Finished" : "Finished with Problems"
    case .needsAttention, .failed: return ""
    }
  }

  /// The trailing glyph of a row that is over or stuck: one family of
  /// circled symbols, so the column reads evenly.
  static func symbol(for item: DownloadItem) -> (name: String, color: Color)? {
    switch item.state {
    case .finished(let summary):
      summary.outcome == .completed ? ("checkmark.circle.fill", .green) : ("exclamationmark.circle.fill", .orange)
    case .needsAttention(.password):
      ("lock.circle.fill", .orange)
    case .needsAttention:
      ("exclamationmark.circle.fill", .orange)
    case .failed:
      ("xmark.circle.fill", .red)
    case .queued, .running, .paused, .stopped:
      nil
    }
  }
}
