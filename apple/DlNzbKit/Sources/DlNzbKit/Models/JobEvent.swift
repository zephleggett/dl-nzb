import Foundation

/// What a job tells its session, in order. Mirrors `dl_nzb::engine::JobEvent`.
public enum JobEvent: Sendable, Equatable {
  case phase(JobPhase)
  /// At most four a second, plus one on every phase change.
  case progress(JobProgress)
  /// After a pre-flight scan.
  case availability(AvailabilityInfo)
  /// One plain-English sentence.
  case warning(String)
  /// Exactly once, always last.
  case finished(JobSummary)
}
