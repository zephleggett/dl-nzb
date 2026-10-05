import DlNzbFFI
import DlNzbKit
import Foundation

/// The real engine: dl-nzb's Rust core through its UniFFI bindings.
///
/// One per app. The Rust side owns a multi-thread runtime, so every call here
/// returns promptly or awaits work running there; file work (`inspect`, the
/// CLI import) runs on a detached task, never on the caller's actor.
///
/// Jobs report through a `JobListener` that does nothing but convert the event
/// and yield it into the session's `AsyncStream`. The engine calls listeners
/// on its own threads, sometimes while holding the job's event lock, so a
/// listener must never call back into the engine; the queue reacts on the main
/// actor instead, when it reads the stream. A Swift task's cancellation does
/// not reach Rust: `JobSession.stop()` is the only way to end a job early.
///
/// Download folder access (Mac sandbox): the job folders live inside the
/// security-scoped folder `SettingsStore` keeps open, and `DownloadQueue`
/// holds that access for each job from start to finish, so a job keeps
/// writing even if the user picks another folder meanwhile. The engine only
/// sees plain paths and needs nothing more.
public final class RustEngine: DownloadEngine {
  private let core: Result<DlNzbFFI.Engine, DlNzbKit.EngineError>

  /// An engine with the default settings and no server; `apply` gives it the user's.
  public init() {
    do {
      core = .success(try DlNzbFFI.Engine(config: DlNzbFFI.EngineConfig(EngineSettings())))
    } catch {
      let failure = DlNzbKit.EngineError(error)
      Log.engine.fault("the engine could not start: \(failure.message, privacy: .public)")
      core = .failure(failure)
    }
  }

  private func engine() throws(DlNzbKit.EngineError) -> DlNzbFFI.Engine {
    try core.get()
  }

  // MARK: DownloadEngine

  public func apply(_ settings: EngineSettings) async throws {
    let config = try DlNzbFFI.EngineConfig(settings)
    do {
      try engine().updateConfig(config: config)
    } catch {
      throw DlNzbKit.EngineError(error)
    }
  }

  public func setSpeedLimit(bytesPerSecond: Int64?) async {
    try? engine().setSpeedLimit(bytesPerSecond: bytesPerSecond.flatMap(UInt64.speedLimit))
  }

  public func testConnection(_ server: ServerSettings, password: String) async throws -> DlNzbKit.ServerCheck {
    let config = try DlNzbFFI.ServerConfig(server, password: password)
    guard !config.host.isEmpty else { throw DlNzbKit.EngineError(.config, "Enter the server's host name.") }
    do {
      return DlNzbKit.ServerCheck(try await engine().testConnection(server: config))
    } catch {
      throw DlNzbKit.EngineError(error)
    }
  }

  public func inspect(_ nzb: URL, fileName: String?) async throws -> DlNzbKit.NzbInfo {
    let engine = try engine()
    return try await Self.detached {
      try nzb.withSecurityScopedAccess {
        DlNzbKit.NzbInfo(try engine.inspect(nzbPath: nzb.path(percentEncoded: false), fileName: fileName))
      }
    }
  }

  public func start(_ request: DlNzbKit.JobRequest) async throws -> JobSession {
    let engine = try engine()
    let ffiRequest = DlNzbFFI.JobRequest(request, freeSpaceHint: Self.freeSpaceHint(for: request.outputDirectory))
    return Self.session { listener in engine.start(request: ffiRequest, listener: listener) }
  }

  public func reprocess(directory: URL, passwords: [String]) async throws -> JobSession {
    let engine = try engine()
    let path = directory.path(percentEncoded: false)
    return Self.session { listener in engine.reprocess(outputDir: path, passwords: passwords, listener: listener) }
  }

  /// Reads a CLI `config.toml` the user picked (an open panel's
  /// security-scoped URL is fine) with the CLI's own parser, so the app and
  /// the CLI never disagree about what the file says.
  public func importCLIConfig(from url: URL) async throws -> ImportedSettings {
    try await Self.detached {
      let imported = try url.withSecurityScopedAccess { try DlNzbFFI.cliConfigImport(path: url.path(percentEncoded: false)) }
      guard let imported else {
        throw DlNzbKit.EngineError(.io, "There is no \(url.lastPathComponent) at \(url.deletingLastPathComponent().path(percentEncoded: false)).")
      }
      return ImportedSettings(imported)
    }
  }

  public func shutdown() async {
    guard let engine = try? engine() else { return }
    await engine.shutdown()
  }

  // MARK: Free space

  /// What the engine's free-space check should use for `folder`, or nil to
  /// let it ask the file system. On iPhone and iPad `statvfs` leaves out
  /// purgeable space (caches the system deletes on demand), which can be
  /// most of a full-looking device, so the capacity for important usage is
  /// passed instead. On the Mac `statvfs` is right.
  public static func freeSpaceHint(for folder: URL) -> UInt64? {
    #if os(iOS)
      importantUsageCapacity(of: folder)
    #else
      nil
    #endif
  }

  /// The space available for important usage on the volume holding `folder`
  /// (or its nearest existing ancestor, as a job folder is created later).
  public static func importantUsageCapacity(of folder: URL) -> UInt64? {
    var candidate = folder.standardizedFileURL
    while !FileManager.default.fileExists(atPath: candidate.path(percentEncoded: false)) {
      let parent = candidate.deletingLastPathComponent()
      if parent.path(percentEncoded: false) == candidate.path(percentEncoded: false) { return nil }
      candidate = parent
    }
    guard let values = try? candidate.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]),
      let capacity = values.volumeAvailableCapacityForImportantUsage, capacity > 0
    else { return nil }
    return UInt64(capacity)
  }

  // MARK: Helpers

  /// Starts a job with a listener feeding a fresh stream, and wraps its handle.
  private static func session(_ start: (Listener) -> DlNzbFFI.JobHandle) -> JobSession {
    let (events, continuation) = AsyncStream.makeStream(of: DlNzbKit.JobEvent.self, bufferingPolicy: .unbounded)
    let handle = start(Listener(continuation))
    return JobSession(events: events, pause: handle.pause, resume: handle.resume, stop: handle.stop)
  }

  /// Blocking work (file reads through the engine) off the caller's actor,
  /// with the bindings' errors as the models' `EngineError`.
  private static func detached<T: Sendable>(_ body: @escaping @Sendable () throws -> T) async throws(DlNzbKit.EngineError) -> T {
    do {
      return try await Task.detached(priority: .userInitiated, operation: body).value
    } catch {
      throw DlNzbKit.EngineError(error)
    }
  }
}

/// Converts each event and yields it, nothing more (see `RustEngine`). Ends
/// the stream after `.finished`, which the engine sends exactly once, last.
private final class Listener: DlNzbFFI.JobListener {
  private let continuation: AsyncStream<DlNzbKit.JobEvent>.Continuation

  init(_ continuation: AsyncStream<DlNzbKit.JobEvent>.Continuation) {
    self.continuation = continuation
  }

  func onEvent(event: DlNzbFFI.JobEvent) {
    let event = DlNzbKit.JobEvent(event)
    continuation.yield(event)
    if case .finished = event {
      continuation.finish()
    }
  }
}
