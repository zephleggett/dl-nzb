import DlNzbKit
import Foundation

/// One step of the inspector's checklist: Check · Download · Verify · Repair · Extract.
public struct PhaseStep: Identifiable, Equatable, Sendable {
  public enum Kind: String, CaseIterable, Sendable {
    case check
    case download
    case verify
    case repair
    case extract

    public var title: String {
      switch self {
      case .check: "Check"
      case .download: "Download"
      case .verify: "Verify"
      case .repair: "Repair"
      case .extract: "Extract"
      }
    }

    /// The step an engine phase belongs to. Connecting comes before the
    /// first step; renaming after the last.
    public init?(_ phase: JobPhase) {
      switch phase {
      case .connecting, .renaming: return nil
      case .checking: self = .check
      case .downloading, .downloadingRecovery: self = .download
      case .verifying: self = .verify
      case .repairing: self = .repair
      case .extracting: self = .extract
      }
    }
  }

  public enum Status: Equatable, Sendable {
    case pending
    /// Running; the fraction when the phase reports one.
    case active(Double?)
    /// Paused part way, or waiting to continue.
    case interrupted(Double?)
    /// Stopped part way by the user.
    case stopped(Double?)
    case done
    /// Not needed, turned off, or nothing to do.
    case skipped
    /// Stopped here waiting for the user.
    case attention
    case failed
  }

  public let kind: Kind
  public let status: Status
  /// "12 blocks", "Not needed", "2 of 5".
  public let detail: String?
  public var id: Kind { kind }

  public init(_ kind: Kind, _ status: Status, detail: String? = nil) {
    self.kind = kind
    self.status = status
    self.detail = detail
  }

  /// The checklist for an item, from its state, the phases it has been
  /// through and, once it is over, its summary.
  public static func steps(for item: DownloadItem, locale: Locale = .autoupdatingCurrent) -> [PhaseStep] {
    let visited = Set(item.visitedPhases.compactMap(Kind.init))
    let order = Kind.allCases

    /// Steps before `index` are done when visited and skipped when not;
    /// `current` is the step at `index`; the rest are pending.
    func around(_ index: Int, current: Status, detail: String? = nil) -> [PhaseStep] {
      order.enumerated().map { offset, kind in
        if offset < index { return PhaseStep(kind, visited.contains(kind) ? .done : .skipped) }
        if offset == index { return PhaseStep(kind, current, detail: detail) }
        return PhaseStep(kind, .pending)
      }
    }

    /// Where the item got to: the latest step it visited.
    var reached: Int? {
      order.lastIndex { visited.contains($0) }
    }

    switch item.state {
    case .queued, .paused:
      guard let reached else { return order.map { PhaseStep($0, .pending) } }
      return around(reached, current: .interrupted(transferFraction(item, at: order[reached])))
    case .stopped:
      guard let reached else { return order.map { PhaseStep($0, .pending) } }
      return around(reached, current: .stopped(transferFraction(item, at: order[reached])))
    case .running(let phase):
      if phase == .renaming {
        return order.map { PhaseStep($0, visited.contains($0) ? .done : .skipped) }
      }
      guard let kind = Kind(phase), let index = order.firstIndex(of: kind) else {
        return order.map { PhaseStep($0, .pending) }
      }
      let progress = item.phaseProgress
      return around(index, current: .active(progress?.displayFraction), detail: progress.flatMap { activeDetail($0, locale: locale) })
    case .needsAttention(.unrepairable):
      return around(0, current: .attention, detail: "Too much missing")
    case .needsAttention(.password):
      let index = order.firstIndex(of: .extract) ?? order.count - 1
      return around(index, current: .attention, detail: "Password required")
    case .needsAttention(.diskFull):
      let index = order.firstIndex(of: .download) ?? 1
      return around(index, current: .attention, detail: "Not enough space")
    case .failed:
      return around(reached ?? 0, current: .failed)
    case .finished(let summary):
      return finished(summary, visited: visited, locale: locale)
    }
  }

  private static func finished(_ summary: JobSummary, visited: Set<Kind>, locale: Locale) -> [PhaseStep] {
    let par2 = summary.par2
    let check = PhaseStep(.check, visited.contains(.check) ? .done : .skipped)
    let download = PhaseStep(.download, .done)
    let verify = PhaseStep(.verify, par2.ran ? .done : .skipped, detail: par2.ran ? nil : par2.skippedReason.map(StatusText.withoutFullStop))
    let repair: PhaseStep
    if par2.repaired || par2.repairedBlocks > 0 {
      repair = PhaseStep(.repair, .done, detail: Format.count(par2.repairedBlocks, "block", "blocks", locale: locale))
    } else if par2.ran && !par2.verifiedOK {
      repair = PhaseStep(.repair, .failed, detail: "Not enough recovery data")
    } else {
      repair = PhaseStep(.repair, .skipped, detail: par2.ran ? "Not needed" : nil)
    }
    let extract: PhaseStep
    if summary.archivesFailed > 0 {
      extract = PhaseStep(.extract, .failed, detail: "\(Format.count(summary.archivesFailed, locale: locale)) failed")
    } else if summary.archivesExtracted > 0 {
      extract = PhaseStep(
        .extract, .done, detail: summary.archivesExtracted > 1 ? Format.count(summary.archivesExtracted, "archive", "archives", locale: locale) : nil)
    } else {
      extract = PhaseStep(.extract, .skipped, detail: visited.contains(.extract) ? nil : "No archives")
    }
    return [check, download, verify, repair, extract]
  }

  private static func transferFraction(_ item: DownloadItem, at kind: Kind) -> Double? {
    guard kind == .download, let progress = item.progress, progress.phase.isTransfer else { return nil }
    return progress.displayFraction
  }

  private static func activeDetail(_ progress: JobProgress, locale: Locale) -> String? {
    switch progress.phase {
    case .repairing where progress.damagedBlocks > 0:
      Format.count(progress.damagedBlocks, "block", "blocks", locale: locale)
    case .extracting where progress.filesTotal > 1:
      Format.count(progress.currentFile, of: progress.filesTotal, locale: locale)
    case .downloadingRecovery:
      "Recovery data"
    default:
      Format.percent(progress.displayFraction, locale: locale)
    }
  }
}
