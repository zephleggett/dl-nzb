import Foundation

/// What a running job is doing, in the order the engine goes through them.
/// Mirrors `dl_nzb::engine::JobPhase`.
public enum JobPhase: String, Sendable, Codable, Hashable, CaseIterable {
  case connecting
  case checking
  case downloading
  case downloadingRecovery
  case verifying
  case repairing
  case extracting
  case renaming

  /// Phases that hold server connections. The queue lets only one job be in
  /// these at a time, so a second download never halves the first one's speed;
  /// post-processing can overlap the next download.
  public var usesNetwork: Bool {
    switch self {
    case .connecting, .checking, .downloading, .downloadingRecovery: true
    case .verifying, .repairing, .extracting, .renaming: false
    }
  }

  /// Phases the engine can pause. Pausing elsewhere either restarts the job
  /// later (connecting, checking) or is not offered (post-processing).
  public var isTransfer: Bool {
    self == .downloading || self == .downloadingRecovery
  }

  /// The phase's name as the inspector and the phase checklist show it.
  public var title: String {
    switch self {
    case .connecting: "Connecting"
    case .checking: "Checking"
    case .downloading: "Downloading"
    case .downloadingRecovery: "Downloading Recovery Data"
    case .verifying: "Verifying"
    case .repairing: "Repairing"
    case .extracting: "Extracting"
    case .renaming: "Renaming"
    }
  }
}
