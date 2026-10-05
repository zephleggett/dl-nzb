import Foundation

/// How a job ended. Mirrors `dl_nzb::engine::Outcome`.
public enum Outcome: String, Sendable, Codable, Equatable {
  case completed
  case completedWithIssues
  case failed
  case stopped
  /// Downloaded, but an archive is encrypted and no password worked.
  /// `DownloadEngine.reprocess` can finish it once the user supplies one.
  case needsPassword
  /// The pre-flight scan found more missing than PAR2 can rebuild, and the
  /// request said to stop rather than download anyway.
  case unrepairable

  /// Whether the files are there for the user to open.
  public var isSuccess: Bool {
    self == .completed || self == .completedWithIssues
  }
}

/// What PAR2 did. Mirrors the engine's `Par2Report`.
public struct Par2Report: Sendable, Codable, Equatable {
  public var ran: Bool
  public var verifiedOK: Bool
  public var damagedBlocks: Int
  public var repairedBlocks: Int
  public var repaired: Bool
  /// Why PAR2 did not run: no recovery files, turned off in settings.
  public var skippedReason: String?

  public init(
    ran: Bool = false, verifiedOK: Bool = false, damagedBlocks: Int = 0, repairedBlocks: Int = 0, repaired: Bool = false, skippedReason: String? = nil
  ) {
    self.ran = ran
    self.verifiedOK = verifiedOK
    self.damagedBlocks = damagedBlocks
    self.repairedBlocks = repairedBlocks
    self.repaired = repaired
    self.skippedReason = skippedReason
  }

  public static let notRun = Par2Report()
}

/// One of the files a finished job leaves in its folder.
public struct OutputFile: Sendable, Codable, Equatable, Hashable {
  public var name: String
  public var bytes: Int64

  public init(name: String, bytes: Int64) {
    self.name = name
    self.bytes = bytes
  }
}

/// Everything a finished job reports, sent once as its last event.
/// Mirrors `dl_nzb::engine::JobSummary`.
public struct JobSummary: Sendable, Codable, Equatable {
  public var outcome: Outcome
  /// One sentence for the UI when the outcome is not `.completed`.
  public var message: String?
  /// Set when the job failed with an `EngineError`, so the queue can tell a
  /// server problem (pause everything, ask for settings) from a job problem.
  public var errorKind: EngineError.Kind?
  public var outputDirectory: URL
  public var files: [OutputFile]
  public var dataBytes: Int64
  public var wireBytes: Int64
  public var elapsedSeconds: Double
  public var downloadSeconds: Double
  public var articlesTotal: Int64
  public var articlesFailed: Int64
  public var par2: Par2Report
  public var archivesExtracted: Int
  public var archivesFailed: Int
  public var filesRenamed: Int
  public var availability: AvailabilityInfo?
  /// Stopped or failed with a sidecar that `start` can continue from.
  public var resumable: Bool

  public init(
    outcome: Outcome,
    message: String? = nil,
    errorKind: EngineError.Kind? = nil,
    outputDirectory: URL,
    files: [OutputFile] = [],
    dataBytes: Int64 = 0,
    wireBytes: Int64 = 0,
    elapsedSeconds: Double = 0,
    downloadSeconds: Double = 0,
    articlesTotal: Int64 = 0,
    articlesFailed: Int64 = 0,
    par2: Par2Report = .notRun,
    archivesExtracted: Int = 0,
    archivesFailed: Int = 0,
    filesRenamed: Int = 0,
    availability: AvailabilityInfo? = nil,
    resumable: Bool = false
  ) {
    self.outcome = outcome
    self.message = message
    self.errorKind = errorKind
    self.outputDirectory = outputDirectory
    self.files = files
    self.dataBytes = dataBytes
    self.wireBytes = wireBytes
    self.elapsedSeconds = elapsedSeconds
    self.downloadSeconds = downloadSeconds
    self.articlesTotal = articlesTotal
    self.articlesFailed = articlesFailed
    self.par2 = par2
    self.archivesExtracted = archivesExtracted
    self.archivesFailed = archivesFailed
    self.filesRenamed = filesRenamed
    self.availability = availability
    self.resumable = resumable
  }

  /// Failed articles as a fraction of all of them.
  public var missingFraction: Double {
    articlesTotal > 0 ? Double(articlesFailed) / Double(articlesTotal) : 0
  }

  /// Average download speed in bytes per second, or nil when nothing was downloaded.
  public var averageSpeed: Double? {
    downloadSeconds > 0 && wireBytes > 0 ? Double(wireBytes) / downloadSeconds : nil
  }
}
