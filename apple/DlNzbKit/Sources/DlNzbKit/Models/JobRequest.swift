import Foundation

/// Whether to scan the server for missing articles before downloading.
/// Mirrors `dl_nzb::engine::Preflight`.
public enum Preflight: String, Sendable, Codable, Equatable, CaseIterable {
  /// The CLI's rule.
  case automatic
  case always
  case never

  public var title: String {
    switch self {
    case .automatic: "Automatic"
    case .always: "Always"
    case .never: "Never"
    }
  }
}

/// What to do when a pre-flight scan finds more missing than PAR2 can rebuild.
/// Mirrors `dl_nzb::engine::OnUnrepairable`.
public enum OnUnrepairable: String, Sendable, Codable, Equatable {
  /// Finish with `Outcome.unrepairable` and let the user decide.
  case stop
  /// Download whatever is there (the user chose Download Anyway).
  case `continue`
}

/// One job for the engine. Mirrors `dl_nzb::engine::JobRequest`.
public struct JobRequest: Sendable, Equatable {
  public var nzbURL: URL
  /// The exact job folder. The queue chooses and de-duplicates it; the engine creates it.
  public var outputDirectory: URL
  /// Tried in order for encrypted archives, after the NZB's own `<meta type="password">`.
  public var passwords: [String]
  public var preflight: Preflight
  public var onUnrepairable: OnUnrepairable
  /// The job's name. Renaming calls an obfuscated main file after it, not after the
  /// de-duplicated folder ("Name 2"). `nil` uses the NZB's own title.
  public var title: String?

  public init(
    nzbURL: URL, outputDirectory: URL, passwords: [String] = [], preflight: Preflight = .automatic, onUnrepairable: OnUnrepairable = .stop,
    title: String? = nil
  ) {
    self.nzbURL = nzbURL
    self.outputDirectory = outputDirectory
    self.passwords = passwords
    self.preflight = preflight
    self.onUnrepairable = onUnrepairable
    self.title = title
  }
}
