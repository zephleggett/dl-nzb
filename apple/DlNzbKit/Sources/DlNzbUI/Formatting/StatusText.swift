import DlNzbKit
import Foundation

/// The one status line under each download, per the SPEC's lifecycle table,
/// and the words around it: the headline of a problem, the window subtitle.
///
/// Parts are joined with a middle dot. Lines never end with a full stop and
/// never say "Loading": they say what is happening.
public enum StatusText {
  static let separator = " · "

  /// What the row says once a password the user typed did not open the
  /// archive, and what the password prompt says above its field.
  public static let passwordRejected = "That password did not work"

  /// The status line for an item in any state. A waiting item the queue is
  /// holding (`DownloadQueue.isHeld`: a server problem, or Pause All) reads
  /// "Paused", as nothing will start it until the hold lifts.
  public static func line(for item: DownloadItem, held: Bool = false, locale: Locale = .autoupdatingCurrent) -> String {
    switch item.state {
    case .queued:
      return joined(held ? "Paused" : "Waiting", size(item, locale))
    case .running(let phase):
      return running(phase, item: item, locale: locale)
    case .paused:
      if let progress = item.progress, progress.phase.isTransfer, progress.bytesTotal > 0 {
        return joined("Paused", Format.bytes(progress.bytesDone, of: progress.bytesTotal, locale: locale))
      }
      return joined("Paused", size(item, locale))
    case .finished(let summary):
      return finished(summary, item: item, locale: locale)
    case .failed(let message, _):
      return failed(message: message, summary: item.summary, locale: locale)
    case .needsAttention(.unrepairable(let availability)):
      return "\(sentenceStart(Format.share(availability.missingFraction, locale: locale))) of articles are missing and there is not enough recovery data"
    case .needsAttention(.password):
      return item.passwordRejected ? passwordRejected : "The archive is encrypted"
    case .needsAttention(.diskFull(let message)):
      return withoutFullStop(message)
    case .stopped:
      if let progress = item.progress, progress.phase.isTransfer, progress.bytesTotal > 0, progress.bytesDone > 0 {
        return joined("Stopped", Format.bytes(progress.bytesDone, of: progress.bytesTotal, locale: locale))
      }
      return "Stopped"
    }
  }

  /// The status line for an item as the queue stands: "Paused" for one it
  /// holds back.
  @MainActor
  public static func line(for item: DownloadItem, in queue: DownloadQueue, locale: Locale = .autoupdatingCurrent) -> String {
    line(for: item, held: queue.isHeld(item), locale: locale)
  }

  private static func running(_ phase: JobPhase, item: DownloadItem, locale: Locale) -> String {
    let progress = item.phaseProgress
    switch phase {
    case .connecting:
      return "Connecting…"
    case .checking:
      let articles = item.info?.articleCount ?? 0
      return articles > 0 ? "Checking \(Format.count(articles, locale: locale)) articles…" : "Checking articles…"
    case .downloading:
      guard let progress, progress.bytesTotal > 0 else { return joined("Downloading", size(item, locale)) }
      var parts = [Format.bytes(progress.bytesDone, of: progress.bytesTotal, locale: locale)]
      if progress.speedBytesPerSecond >= 1 {
        parts.append(Format.speed(progress.speedBytesPerSecond, locale: locale))
        // The engine leaves the estimate out until the speed has settled.
        if let eta = progress.etaSeconds { parts.append(Format.timeLeft(eta, locale: locale)) }
      } else if progress.bytesDone < progress.bytesTotal {
        // Nothing arriving: connecting again, or a slow server.
        parts.append("Waiting for the server…")
      }
      return parts.joined(separator: separator)
    case .downloadingRecovery:
      guard let progress, progress.bytesTotal > 0 else { return "Downloading recovery data" }
      return joined("Downloading recovery data", Format.bytes(progress.bytesDone, of: progress.bytesTotal, locale: locale))
    case .verifying:
      return joined("Verifying", progress.map { Format.percent($0.fraction, locale: locale) })
    case .repairing:
      let blocks = progress?.damagedBlocks ?? 0
      let title = blocks > 0 ? "Repairing \(Format.count(blocks, "damaged block", "damaged blocks", locale: locale))" : "Repairing"
      return joined(title, progress.map { Format.percent($0.fraction, locale: locale) })
    case .extracting:
      guard let progress else { return "Extracting" }
      if progress.filesTotal > 1 {
        return joined("Extracting", Format.count(progress.currentFile, of: progress.filesTotal, locale: locale))
      }
      return joined("Extracting", Format.percent(progress.fraction, locale: locale))
    case .renaming:
      return "Renaming files"
    }
  }

  private static func finished(_ summary: JobSummary, item: DownloadItem, locale: Locale) -> String {
    var parts = [Format.bytes(item.displayBytes, locale: locale), "Finished in \(Format.duration(summary.elapsedSeconds, locale: locale))"]
    if summary.par2.repairedBlocks > 0 {
      parts.append("Repaired \(Format.count(summary.par2.repairedBlocks, "block", "blocks", locale: locale))")
    }
    if summary.outcome == .completedWithIssues {
      if summary.archivesFailed > 0 {
        parts.append(
          Format.count(summary.archivesFailed, "archive could not be extracted", "archives could not be extracted", locale: locale))
      } else if summary.par2.ran && !summary.par2.verifiedOK && !summary.par2.repaired {
        parts.append("Some files damaged")
      } else {
        parts.append("With problems")
      }
    }
    return parts.joined(separator: separator)
  }

  /// "9% of articles missing" when missing articles are the story, otherwise
  /// the engine's sentence.
  private static func failed(message: String, summary: JobSummary?, locale: Locale) -> String {
    if let summary, summary.errorKind == nil, summary.articlesTotal > 0, summary.missingFraction >= 0.005 {
      return "\(sentenceStart(Format.share(summary.missingFraction, locale: locale))) of articles missing"
    }
    let trimmed = withoutFullStop(message)
    return trimmed.isEmpty ? "Failed" : trimmed
  }

  /// A word or two above the line for the states that ask something of the user.
  public static func headline(for item: DownloadItem) -> String? {
    switch item.state {
    case .needsAttention(.unrepairable): "Needs Attention"
    case .needsAttention(.password): "Password Required"
    case .needsAttention(.diskFull): "Not Enough Space"
    case .failed: "Failed"
    default: nil
    }
  }

  /// What Retry is called for an item: "Download Again" when it would start
  /// over (a failure the engine cannot continue, or a stop whose data was
  /// deleted), "Retry" when it continues from what it has.
  public static func retryTitle(for item: DownloadItem) -> String {
    switch item.state {
    case .failed(_, let resumable): resumable ? "Retry" : "Download Again"
    case .stopped: item.hasData ? "Retry" : "Download Again"
    default: "Retry"
    }
  }

  /// How the line and the item's glyph are coloured. Words take the tone's
  /// `textStyle` (lines) or `emphasisStyle` (headlines); glyphs its
  /// `glyphStyle`.
  public static func tone(for item: DownloadItem) -> StatusTone {
    switch item.state {
    case .queued, .paused, .stopped: .neutral
    case .running: .active
    case .finished(let summary): summary.outcome == .completed ? .good : .warning
    case .needsAttention: .warning
    case .failed: .bad
    }
  }

  /// The window subtitle and menu bar summary: "1 downloading · 84 MB/s",
  /// "1 downloading · 84 MB/s · 1 extracting", "1 verifying",
  /// "Paused · 3 waiting", "3 waiting", or empty when there is nothing to say.
  ///
  /// - Parameters:
  ///   - downloading: Jobs in a network phase.
  ///   - processing: The phases of jobs past their download (verifying,
  ///     repairing, extracting, renaming), named when they all agree.
  public static func queueSummary(
    downloading: Int, processing: [JobPhase] = [], queued: Int, paused: Int, isPaused: Bool, speed: Double,
    locale: Locale = .autoupdatingCurrent
  ) -> String {
    var parts: [String?] = []
    if downloading > 0 {
      parts.append("\(Format.count(downloading, locale: locale)) downloading")
      parts.append(speed >= 1 ? Format.speed(speed, locale: locale) : nil)
    } else if isPaused || paused > 0 {
      parts.append("Paused")
    }
    parts.append(processingSummary(processing, locale: locale))
    if downloading == 0 && queued > 0 {
      parts.append("\(Format.count(queued, locale: locale)) waiting")
    }
    return parts.compactMap { $0 }.joined(separator: separator)
  }

  /// `queueSummary` for the queue as it stands, leaving out `excluded` (the
  /// item the menu bar names above it). The reader updates with the speed.
  @MainActor
  public static func queueSummary(
    for queue: DownloadQueue, excluding excluded: DownloadItem.ID? = nil, locale: Locale = .autoupdatingCurrent
  ) -> String {
    let items = queue.items.filter { $0.id != excluded }
    return queueSummary(
      downloading: items.count(where: \.usesNetwork),
      processing: items.compactMap { $0.phase.flatMap { $0.usesNetwork ? nil : $0 } },
      queued: items.count(where: \.isQueued), paused: items.count(where: \.isPaused), isPaused: queue.isPaused,
      speed: queue.speed(excluding: excluded), locale: locale)
  }

  /// "1 extracting", "2 verifying", or "2 processing" when they differ.
  private static func processingSummary(_ phases: [JobPhase], locale: Locale) -> String? {
    guard let first = phases.first else { return nil }
    let word: String
    if phases.allSatisfy({ $0 == first }) {
      switch first {
      case .verifying: word = "verifying"
      case .repairing: word = "repairing"
      case .extracting: word = "extracting"
      default: word = "processing"
      }
    } else {
      word = "processing"
    }
    return "\(Format.count(phases.count, locale: locale)) \(word)"
  }

  // MARK: Helpers

  private static func size(_ item: DownloadItem, _ locale: Locale) -> String? {
    item.totalBytes > 0 ? Format.bytes(item.totalBytes, locale: locale) : nil
  }

  private static func joined(_ parts: String?...) -> String {
    parts.compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: separator)
  }

  static func withoutFullStop(_ text: String) -> String {
    var trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
    while trimmed.hasSuffix(".") { trimmed.removeLast() }
    return trimmed
  }

  /// "less than 1%" at the start of a sentence.
  private static func sentenceStart(_ text: String) -> String {
    text.prefix(1).uppercased() + text.dropFirst()
  }
}
