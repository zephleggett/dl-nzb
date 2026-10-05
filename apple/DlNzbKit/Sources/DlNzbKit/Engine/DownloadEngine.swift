import Foundation

/// The download engine as the stores see it. `RustEngine` (DlNzbRust) wraps the
/// real one; `SimulatedEngine` stands in for previews, tests and `-simulate YES`.
///
/// Every method may block on file or network work, so callers on the main actor
/// only ever await them; implementations do the work elsewhere.
public protocol DownloadEngine: AnyObject, Sendable {
  /// New settings, for jobs started afterwards. Throws `EngineError(.config)`
  /// when they cannot work (no host, a connection count out of range).
  func apply(_ settings: EngineSettings) async throws
  /// Engine-wide and live; nil is unlimited.
  func setSpeedLimit(bytesPerSecond: Int64?) async
  /// Connects and logs in, and says exactly what went wrong when it cannot.
  func testConnection(_ server: ServerSettings, password: String) async throws -> ServerCheck
  /// Reads an NZB without touching the network.
  func inspect(_ nzb: URL) async throws -> NzbInfo
  /// Starts a job. Its folder may hold a sidecar from an earlier run, which it continues.
  func start(_ request: JobRequest) async throws -> JobSession
  /// Post-processing only, for a downloaded job that needed a password.
  func reprocess(directory: URL, passwords: [String]) async throws -> JobSession
  /// The dl-nzb CLI's settings, where the engine can read them.
  func importCLIConfig() async -> ImportedSettings?
  /// A CLI config file the user picked (the sandboxed Mac app's open panel).
  /// `RustEngine` reads it with the CLI's own parser, the one source of truth
  /// for what the file means; the default here is `CLIConfig`, the Swift
  /// reader the simulated engine and previews use.
  func importCLIConfig(from url: URL) async throws -> ImportedSettings
  /// Stops every job so it can resume, and closes the server connections.
  func shutdown() async
}

extension DownloadEngine {
  public func importCLIConfig(from url: URL) async throws -> ImportedSettings {
    try await Task.detached(priority: .userInitiated) { try CLIConfig.read(from: url) }.value
  }
}

/// One running job: its events, and the three things the user can do to it.
///
/// `events` is single-consumer and ends after `.finished`. The controls are
/// plain closures so any engine can make one: the Rust adapter wraps its
/// `JobHandle`, the simulated engine its own job, a test whatever it likes.
///
/// ```swift
/// let (events, continuation) = AsyncStream.makeStream(of: JobEvent.self, bufferingPolicy: .unbounded)
/// let handle = engine.start(request, listener: Listener(continuation))
/// return JobSession(events: events, pause: handle.pause, resume: handle.resume, stop: handle.stop)
/// ```
public final class JobSession: Sendable {
  public let events: AsyncStream<JobEvent>
  private let onPause: @Sendable () -> Void
  private let onResume: @Sendable () -> Void
  private let onStop: @Sendable () -> Void

  public init(
    events: AsyncStream<JobEvent>,
    pause: @escaping @Sendable () -> Void,
    resume: @escaping @Sendable () -> Void,
    stop: @escaping @Sendable () -> Void
  ) {
    self.events = events
    self.onPause = pause
    self.onResume = resume
    self.onStop = stop
  }

  /// Download phases only: the job releases its connections and waits.
  public func pause() { onPause() }

  public func resume() { onResume() }

  /// Ends the job promptly. Its data and sidecar stay, so `start` continues it;
  /// the last event is `.finished` with `Outcome.stopped`.
  public func stop() { onStop() }
}
