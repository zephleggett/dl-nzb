import Foundation

/// A snapshot of a running job, sent at most four times a second and once on
/// every phase change. Mirrors `dl_nzb::engine::JobProgress`.
///
/// Sizes are `Int64` rather than the engine's `u64` so they go straight into
/// `ByteCountFormatStyle`; the adapter converts.
public struct JobProgress: Sendable, Codable, Equatable {
  public var phase: JobPhase
  /// Download phases: decoded payload bytes of this phase's file set.
  public var bytesDone: Int64
  public var bytesTotal: Int64
  /// Per-job wire speed, smoothed over about two seconds.
  public var speedBytesPerSecond: Double
  public var etaSeconds: Int64?
  public var filesDone: Int
  public var filesTotal: Int
  public var articlesFailed: Int64
  /// 0...1 within the current phase.
  public var fraction: Double
  /// "2 of 5", or the archive or file being worked on.
  public var detail: String?
  public var paused: Bool
  /// Repairing: the damaged blocks being rebuilt, so the status line can say
  /// "Repairing 12 damaged blocks". Zero in every other phase.
  public var damagedBlocks: Int

  public init(
    phase: JobPhase,
    bytesDone: Int64 = 0,
    bytesTotal: Int64 = 0,
    speedBytesPerSecond: Double = 0,
    etaSeconds: Int64? = nil,
    filesDone: Int = 0,
    filesTotal: Int = 0,
    articlesFailed: Int64 = 0,
    fraction: Double = 0,
    detail: String? = nil,
    paused: Bool = false,
    damagedBlocks: Int = 0
  ) {
    self.phase = phase
    self.bytesDone = bytesDone
    self.bytesTotal = bytesTotal
    self.speedBytesPerSecond = speedBytesPerSecond
    self.etaSeconds = etaSeconds
    self.filesDone = filesDone
    self.filesTotal = filesTotal
    self.articlesFailed = articlesFailed
    self.fraction = fraction
    self.detail = detail
    self.paused = paused
    self.damagedBlocks = damagedBlocks
  }

  /// Bytes as a fraction when the phase moves bytes, otherwise the phase's
  /// own fraction, clamped to 0...1.
  public var displayFraction: Double {
    let value = phase.isTransfer && bytesTotal > 0 ? Double(bytesDone) / Double(bytesTotal) : fraction
    return value.isFinite ? min(max(value, 0), 1) : 0
  }

  /// The file being worked on, counting from one: the 2 of "2 of 5".
  public var currentFile: Int {
    min(filesDone + 1, filesTotal)
  }
}
