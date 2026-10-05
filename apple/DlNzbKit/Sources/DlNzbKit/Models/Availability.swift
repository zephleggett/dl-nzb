import Foundation

/// What a pre-flight scan found on the server. Mirrors `dl_nzb::engine::AvailabilityInfo`.
public struct AvailabilityInfo: Sendable, Codable, Equatable {
  /// Mirrors `dl_nzb::engine::Verdict`. Nested so it never collides with the
  /// generated bindings' top-level `Verdict` in the adapter.
  public enum Verdict: String, Sendable, Codable, Equatable {
    case complete
    case repairable
    case unrepairable
    case unknown
  }

  public var articlesTotal: Int64
  public var articlesMissing: Int64
  public var missingBytes: Int64
  public var recoveryBytes: Int64
  public var verdict: Verdict

  public init(articlesTotal: Int64, articlesMissing: Int64, missingBytes: Int64, recoveryBytes: Int64, verdict: Verdict) {
    self.articlesTotal = articlesTotal
    self.articlesMissing = articlesMissing
    self.missingBytes = missingBytes
    self.recoveryBytes = recoveryBytes
    self.verdict = verdict
  }

  /// Missing articles as a fraction of all of them, 0 when nothing was scanned.
  public var missingFraction: Double {
    articlesTotal > 0 ? Double(articlesMissing) / Double(articlesTotal) : 0
  }
}
