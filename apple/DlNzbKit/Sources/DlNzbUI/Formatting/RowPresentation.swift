import DlNzbKit
import Foundation

/// The bar under a row's title, per the SPEC's lifecycle table: bytes while
/// moving bytes, the phase's fraction while processing, indeterminate only
/// while connecting and checking, and none at all once it is over. A job never
/// alternates between a spinner and a bar within a phase.
public enum RowProgress: Equatable, Sendable {
  case none
  case indeterminate
  case determinate(Double)
  /// Paused: the bar stays where it was, in a secondary tint.
  case frozen(Double)

  public static func of(_ item: DownloadItem) -> RowProgress {
    switch item.state {
    case .queued, .finished, .failed, .needsAttention, .stopped:
      return .none
    case .paused:
      return .frozen(item.downloadFraction)
    case .running(.connecting), .running(.checking):
      return .indeterminate
    case .running:
      return .determinate(item.phaseProgress?.displayFraction ?? 0)
    }
  }
}

/// The glyph beside a row whose job is over, stopped or stuck, coloured by
/// its tone.
public enum StatusSymbol {
  /// SF Symbol and tone, or nil for states shown by the bar instead.
  public static func of(_ item: DownloadItem) -> (name: String, tone: StatusTone)? {
    switch item.state {
    case .finished(let summary):
      summary.outcome == .completed ? ("checkmark.circle.fill", .good) : ("exclamationmark.triangle.fill", .warning)
    case .failed:
      ("xmark.circle.fill", .bad)
    case .needsAttention(.password):
      ("lock.fill", .warning)
    case .needsAttention(.diskFull):
      ("externaldrive.badge.exclamationmark", .warning)
    case .needsAttention(.unrepairable):
      ("exclamationmark.triangle.fill", .warning)
    case .stopped:
      ("stop.circle.fill", .neutral)
    case .queued, .running, .paused:
      nil
    }
  }
}

extension ContentKind {
  /// The SF Symbol for the row's icon. All exist in SF Symbols 6 and 7.
  public var symbolName: String {
    switch self {
    case .video: "film"
    case .audio: "music.note"
    case .archive: "zipper.page"
    case .image: "photo"
    case .document: "text.document"
    case .software: "shippingbox"
    case .other: "document"
    }
  }

  /// For VoiceOver.
  public var accessibilityName: String {
    switch self {
    case .video: "Video"
    case .audio: "Audio"
    case .archive: "Archive"
    case .image: "Images"
    case .document: "Document"
    case .software: "Software"
    case .other: "Files"
    }
  }
}

/// A label and a value for the inspector's Details and the iPhone's detail
/// screen, which show the same facts.
public struct DetailRow: Identifiable, Equatable, Sendable {
  public let label: String
  public let value: String
  public var id: String { label }

  public init(_ label: String, _ value: String) {
    self.label = label
    self.value = value
  }

  /// The facts worth showing for an item, in order; rows with nothing to say
  /// are left out.
  public static func rows(for item: DownloadItem, locale: Locale = .autoupdatingCurrent) -> [DetailRow] {
    var rows: [DetailRow] = []
    if item.totalBytes > 0 { rows.append(DetailRow("Size", Format.bytes(item.totalBytes, locale: locale))) }
    if let summary = item.summary, summary.outcome.isSuccess || summary.outcome == .failed {
      if let speed = summary.averageSpeed { rows.append(DetailRow("Average Speed", Format.speed(speed, locale: locale))) }
      if summary.elapsedSeconds > 0 { rows.append(DetailRow("Time", Format.duration(summary.elapsedSeconds, locale: locale))) }
      if summary.articlesTotal > 0 {
        let missing = summary.articlesFailed == 0 ? "None" : Format.count(Int(summary.articlesFailed), of: Int(summary.articlesTotal), locale: locale)
        rows.append(DetailRow("Articles Missing", missing))
      }
      if summary.par2.repairedBlocks > 0 {
        rows.append(DetailRow("Blocks Repaired", Format.count(summary.par2.repairedBlocks, locale: locale)))
      } else if let reason = summary.par2.skippedReason {
        rows.append(DetailRow("Repair", StatusText.withoutFullStop(reason)))
      }
    } else if let availability = item.availability, availability.articlesTotal > 0 {
      rows.append(DetailRow("Articles Missing", Format.count(Int(availability.articlesMissing), of: Int(availability.articlesTotal), locale: locale)))
    }
    if let category = item.info?.category, !category.isEmpty { rows.append(DetailRow("Category", category)) }
    if let info = item.info, !info.passwords.isEmpty {
      rows.append(DetailRow("Password", "In the NZB"))
    } else if !item.passwords.isEmpty {
      rows.append(DetailRow("Password", "Entered"))
    }
    let dateStyle = Date.FormatStyle(date: .abbreviated, time: .shortened).locale(locale)
    rows.append(DetailRow("Added", item.addedAt.formatted(dateStyle)))
    if let finishedAt = item.finishedAt {
      rows.append(DetailRow("Finished", finishedAt.formatted(dateStyle)))
    }
    return rows
  }
}
