import Foundation
import Synchronization

/// A `DownloadEngine` with no network: jobs go through the real phases at a
/// believable pace (speed wobbling around a mean, a ramp at the start, a speed
/// limit honoured), and end in whichever outcome their name asks for (see
/// `SimulatedScenario`). Previews, the Swift tests and `-simulate YES` use it.
///
/// It reads real NZBs, so the queue shows real titles, sizes and article
/// counts, and it keeps a sidecar in the job folder, so stopping and starting a
/// job again continues where it was, as the real engine does. The files it
/// "downloads" are empty placeholders with the right names.
public final class SimulatedEngine: DownloadEngine {
  public struct Configuration: Sendable, Equatable {
    public var meanBytesPerSecond: Double
    /// How far the speed wanders from the mean, as a share of it.
    public var jitter: Double
    /// Simulated seconds per real second. Tests run at thousands.
    public var timeScale: Double
    /// Simulated seconds between progress events: the engine's 4 Hz.
    public var tick: Double
    public var connectSeconds: Double
    public var articlesCheckedPerSecond: Double
    public var verifyBytesPerSecond: Double
    public var repairSecondsPerBlock: Double
    public var extractBytesPerSecond: Double
    public var renameSeconds: Double
    public var latencyMilliseconds: Int
    /// Create the job folder, the sidecar and placeholder files.
    public var writesFiles: Bool

    public init(
      meanBytesPerSecond: Double = 84_000_000,
      jitter: Double = 0.2,
      timeScale: Double = 1,
      tick: Double = 0.25,
      connectSeconds: Double = 0.6,
      articlesCheckedPerSecond: Double = 30_000,
      verifyBytesPerSecond: Double = 1_600_000_000,
      repairSecondsPerBlock: Double = 0.08,
      extractBytesPerSecond: Double = 1_100_000_000,
      renameSeconds: Double = 0.4,
      latencyMilliseconds: Int = 38,
      writesFiles: Bool = true
    ) {
      self.meanBytesPerSecond = meanBytesPerSecond
      self.jitter = jitter
      self.timeScale = timeScale
      self.tick = tick
      self.connectSeconds = connectSeconds
      self.articlesCheckedPerSecond = articlesCheckedPerSecond
      self.verifyBytesPerSecond = verifyBytesPerSecond
      self.repairSecondsPerBlock = repairSecondsPerBlock
      self.extractBytesPerSecond = extractBytesPerSecond
      self.renameSeconds = renameSeconds
      self.latencyMilliseconds = latencyMilliseconds
      self.writesFiles = writesFiles
    }

    /// Real time: an 8 GB release takes a minute and a half at 84 MB/s.
    public static let realistic = Configuration()

    /// For tests: the same phases and events, thousands of times faster.
    public static func fast(timeScale: Double = 20_000, writesFiles: Bool = true) -> Configuration {
      Configuration(timeScale: timeScale, writesFiles: writesFiles)
    }
  }

  /// How the pretend server treats every job and Test Connection.
  public enum ServerBehaviour: String, Sendable, CaseIterable {
    case healthy
    case rejectsLogin
    case unreachable
  }

  private struct State {
    var settings = EngineSettings()
    var speedLimit: Int64?
    var serverBehaviour: ServerBehaviour
    var scenarioOverride: SimulatedScenario?
    var jobs: [UUID: SimulatedJob] = [:]
    var tasks: [UUID: Task<Void, Never>] = [:]
    var startedJobs = 0
    var peakNetworkJobs = 0
  }

  public let configuration: Configuration
  private let state: Mutex<State>

  public init(configuration: Configuration = .realistic, serverBehaviour: ServerBehaviour = .healthy) {
    self.configuration = configuration
    self.state = Mutex(State(serverBehaviour: serverBehaviour))
  }

  // MARK: Knobs for tests and demos

  public var serverBehaviour: ServerBehaviour {
    get { state.withLock { $0.serverBehaviour } }
    set { state.withLock { $0.serverBehaviour = newValue } }
  }

  /// Every job plays this scenario, whatever its name says.
  public var scenarioOverride: SimulatedScenario? {
    get { state.withLock { $0.scenarioOverride } }
    set { state.withLock { $0.scenarioOverride = newValue } }
  }

  /// What `apply` last received.
  public var settings: EngineSettings {
    state.withLock { $0.settings }
  }

  public var speedLimit: Int64? {
    state.withLock { $0.speedLimit }
  }

  /// Jobs started and not yet finished.
  public var runningJobCount: Int {
    state.withLock { $0.jobs.count }
  }

  /// Jobs started over the engine's life, reprocessing included.
  public var startedJobCount: Int {
    state.withLock { $0.startedJobs }
  }

  /// The most jobs that were ever in network phases at once. The queue lets
  /// one at a time in, so tests expect 1.
  public var peakNetworkJobCount: Int {
    state.withLock { $0.peakNetworkJobs }
  }

  /// Jobs moving bytes right now, which share the speed limit.
  var transferringJobCount: Int {
    state.withLock { $0.jobs.values.filter(\.isTransferring).count }
  }

  /// A job entered or left the network phases.
  func networkChanged() {
    state.withLock { state in
      let now = state.jobs.values.filter(\.usesNetwork).count
      state.peakNetworkJobs = max(state.peakNetworkJobs, now)
    }
  }

  // MARK: DownloadEngine

  public func apply(_ settings: EngineSettings) async throws {
    guard ServerSettings.connectionRange.contains(settings.server.connections) else {
      throw EngineError(.config, "Connections must be between 1 and 100.")
    }
    state.withLock {
      $0.settings = settings
      $0.speedLimit = settings.speedLimitBytesPerSecond
    }
  }

  public func setSpeedLimit(bytesPerSecond: Int64?) async {
    state.withLock { $0.speedLimit = bytesPerSecond.flatMap { $0 > 0 ? $0 : nil } }
  }

  public func testConnection(_ server: ServerSettings, password: String) async throws -> ServerCheck {
    try? await Task.sleep(for: .seconds(min(configuration.connectSeconds / configuration.timeScale, 2)))
    let host = server.normalisedHost
    guard !host.isEmpty else { throw EngineError(.config, "Enter the server’s host name.") }
    if host.lowercased().hasSuffix(".invalid") || host.lowercased().contains("invalid.") {
      throw EngineError(.dns, "No server called \(host) could be found. Check the host name.")
    }
    switch serverBehaviour {
    case .unreachable:
      throw EngineError(.connect, "\(host) could not be reached on port \(server.port).")
    case .rejectsLogin:
      throw EngineError(.auth)
    case .healthy:
      break
    }
    if password.lowercased().contains("wrong") || server.username.isEmpty {
      throw EngineError(.auth)
    }
    return ServerCheck(
      greeting: "200 \(host) NNRP Service Ready (posting ok)", tls: server.useSSL, latencyMilliseconds: configuration.latencyMilliseconds)
  }

  public func inspect(_ nzb: URL, fileName: String?) async throws -> NzbInfo {
    try NzbParser.parse(contentsOf: nzb, fileName: fileName)
  }

  public func start(_ request: JobRequest) async throws -> JobSession {
    var parsed = try NzbParser.parse(contentsOf: request.nzbURL)
    // Without a title of its own, an NZB is named after its file, which for
    // the queue's copy is the item's id; the job's folder carries the release
    // name, so the extracted file is named after that instead.
    if UUID(uuidString: parsed.title) != nil {
      parsed.title = request.outputDirectory.lastPathComponent
    }
    let info = parsed
    let fingerprint = (try? Data(contentsOf: request.nzbURL)).map(NzbFingerprint.of) ?? ""
    let (settings, override) = state.withLock { ($0.settings, $0.scenarioOverride) }
    let scenario = override ?? SimulatedScenario(hint: request.outputDirectory.lastPathComponent + " " + info.title)
    return launch { job in
      var run = SimulatedRun(
        job: job, engine: self, configuration: self.configuration, settings: settings, scenario: scenario, info: info, request: request,
        fingerprint: fingerprint)
      return await run.download()
    }
  }

  public func reprocess(directory: URL, passwords: [String]) async throws -> JobSession {
    let sidecar = SimulatedSidecar.read(in: directory)
    let info = sidecar?.info ?? NzbInfo(title: directory.lastPathComponent, totalBytes: 0, dataBytes: 0, par2Bytes: 0)
    let (settings, override) = state.withLock { ($0.settings, $0.scenarioOverride) }
    let scenario = override ?? sidecar?.scenario ?? SimulatedScenario(hint: directory.lastPathComponent)
    let request = JobRequest(nzbURL: directory.appending(path: "reprocess.nzb"), outputDirectory: directory, passwords: passwords)
    return launch { job in
      var run = SimulatedRun(
        job: job, engine: self, configuration: self.configuration, settings: settings, scenario: scenario, info: info, request: request,
        fingerprint: sidecar?.fingerprint ?? "")
      return await run.reprocess()
    }
  }

  public func shutdown() async {
    let (jobs, tasks) = state.withLock { (Array($0.jobs.values), Array($0.tasks.values)) }
    for job in jobs { job.stop() }
    for task in tasks { await task.value }
  }

  // MARK: Jobs

  /// Registers a job, runs `body` on its own task, and hands back the session.
  private func launch(_ body: @escaping @Sendable (SimulatedJob) async -> JobSummary) -> JobSession {
    let (events, continuation) = AsyncStream.makeStream(of: JobEvent.self, bufferingPolicy: .unbounded)
    let job = SimulatedJob(continuation: continuation)
    let id = job.id
    state.withLock {
      $0.jobs[id] = job
      $0.startedJobs += 1
    }
    let task = Task.detached(priority: .utility) { [weak self] in
      let summary = await body(job)
      job.setUsesNetwork(false)
      continuation.yield(.finished(summary))
      continuation.finish()
      self?.state.withLock {
        $0.jobs[id] = nil
        $0.tasks[id] = nil
      }
    }
    state.withLock {
      // A job this quick may already be gone; only a running one is kept.
      if $0.jobs[id] != nil { $0.tasks[id] = task }
    }
    return JobSession(events: events, pause: { job.pause() }, resume: { job.resume() }, stop: { job.stop() })
  }
}

/// One simulated job's controls and event channel. The run reads the flags
/// between ticks; the session's closures set them from any thread.
final class SimulatedJob: Sendable {
  private struct Control {
    var paused = false
    var stopped = false
    var transferring = false
    var usesNetwork = false
  }

  let id = UUID()
  private let control = Mutex(Control())
  private let continuation: AsyncStream<JobEvent>.Continuation

  init(continuation: AsyncStream<JobEvent>.Continuation) {
    self.continuation = continuation
  }

  func emit(_ event: JobEvent) {
    continuation.yield(event)
  }

  /// Only a job moving bytes can pause, as with the real engine.
  func pause() {
    control.withLock { if $0.transferring { $0.paused = true } }
  }

  func resume() {
    control.withLock { $0.paused = false }
  }

  func stop() {
    control.withLock { $0.stopped = true }
  }

  var isPaused: Bool { control.withLock { $0.paused } }
  var isStopped: Bool { control.withLock { $0.stopped } }
  var isTransferring: Bool { control.withLock { $0.transferring && !$0.paused } }
  /// In a network phase and not paused (a paused job lets its connections go).
  var usesNetwork: Bool { control.withLock { $0.usesNetwork && !$0.paused } }

  func setUsesNetwork(_ value: Bool) {
    control.withLock { $0.usesNetwork = value }
  }

  func setTransferring(_ value: Bool) {
    control.withLock {
      $0.transferring = value
      if !value { $0.paused = false }
    }
  }
}

/// What a simulated job leaves in its folder so a later start continues it
/// and a later reprocess knows what it was. Not the real engine's sidecar,
/// and named apart from it, so neither engine misreads the other's.
struct SimulatedSidecar: Codable {
  static let fileName = ".dl-nzb-simulated.json"

  var fingerprint: String
  var scenario: SimulatedScenario
  var info: NzbInfo
  var bytesDone: Int64 = 0
  var downloaded = false

  static func read(in directory: URL) -> SimulatedSidecar? {
    guard let data = try? Data(contentsOf: directory.appending(path: fileName)) else { return nil }
    return try? JSONDecoder().decode(SimulatedSidecar.self, from: data)
  }

  func write(in directory: URL) {
    guard let data = try? JSONEncoder().encode(self) else { return }
    try? data.write(to: directory.appending(path: SimulatedSidecar.fileName), options: .atomic)
  }
}

/// One run of a simulated job, from connecting to its summary. A value with
/// the run's own clock, speed and counters; the job holds the controls.
struct SimulatedRun {
  let job: SimulatedJob
  let engine: SimulatedEngine
  let configuration: SimulatedEngine.Configuration
  let settings: EngineSettings
  let scenario: SimulatedScenario
  let info: NzbInfo
  let request: JobRequest
  let fingerprint: String

  private var generator: SeededGenerator
  private var sidecar: SimulatedSidecar
  private var phase: JobPhase = .connecting
  private var elapsed: Double = 0
  private var downloadSeconds: Double = 0
  private var speedFactor: Double = 1
  private var smoothedSpeed: Double = 0
  private var wireBytes: Int64 = 0
  private var articlesFailed: Int64 = 0
  private var par2 = Par2Report.notRun

  init(
    job: SimulatedJob, engine: SimulatedEngine, configuration: SimulatedEngine.Configuration, settings: EngineSettings, scenario: SimulatedScenario,
    info: NzbInfo, request: JobRequest, fingerprint: String
  ) {
    self.job = job
    self.engine = engine
    self.configuration = configuration
    self.settings = settings
    self.scenario = scenario
    self.info = info
    self.request = request
    self.fingerprint = fingerprint
    self.generator = SeededGenerator(seed: StableHash.of(info.title))
    if let previous = SimulatedSidecar.read(in: request.outputDirectory), previous.fingerprint == fingerprint, !fingerprint.isEmpty {
      self.sidecar = previous
    } else {
      self.sidecar = SimulatedSidecar(fingerprint: fingerprint, scenario: scenario, info: info)
    }
  }

  // MARK: The two kinds of run

  mutating func download() async -> JobSummary {
    if configuration.writesFiles {
      do {
        try FileManager.default.createDirectory(at: request.outputDirectory, withIntermediateDirectories: true)
      } catch {
        return failure(EngineError(.io, "dl-nzb could not create the folder \(request.outputDirectory.lastPathComponent)."))
      }
    }

    enter(.connecting)
    guard await pass(configuration.connectSeconds) else { return stopped() }
    let host = settings.server.normalisedHost.isEmpty ? "the server" : settings.server.normalisedHost
    switch (engine.serverBehaviour, scenario) {
    case (.rejectsLogin, _), (_, .authFailure):
      return failure(EngineError(.auth))
    case (.unreachable, _), (_, .unreachable):
      return failure(EngineError(.connect, "\(host) could not be reached. Check your connection and the server’s port."))
    default:
      break
    }
    if scenario == .diskFull {
      let shortfall = Int64(Double(info.totalBytes) * 0.38)
      return failure(EngineError(.diskFull, "This download needs \(shortfall.formatted(.byteCount(style: .file))) more free space."))
    }

    if !sidecar.downloaded && sidecar.bytesDone == 0 && request.preflight != .never {
      guard await check() else { return stopped() }
      let availability = availabilityReport()
      job.emit(.availability(availability))
      if availability.verdict == .unrepairable && request.onUnrepairable == .stop {
        return summary(.unrepairable, message: Self.missingSentence(availability.missingFraction), availability: availability)
      }
    }

    if !sidecar.downloaded {
      let recoveryUpFront = settings.advanced.downloadAllRecoveryUpFront
      let total = info.dataBytes + (recoveryUpFront ? info.par2Bytes : 0)
      guard await transfer(.downloading, total: total, from: sidecar.bytesDone) else { return stopped() }
      if scenario.losesArticles && !recoveryUpFront && settings.processing.repairWithPar2 && info.par2Bytes > 0 {
        let recovery = max(Int64(Double(info.par2Bytes) * (scenario == .repair ? 0.35 : 1)), 1)
        guard await transfer(.downloadingRecovery, total: recovery, from: 0) else { return stopped() }
      }
      sidecar.downloaded = true
      saveSidecar()
    }
    return await postProcess(passwords: info.passwords + request.passwords)
  }

  mutating func reprocess() async -> JobSummary {
    await postProcess(passwords: info.passwords + request.passwords, repairing: false)
  }

  // MARK: Phases

  private mutating func check() async -> Bool {
    enter(.checking)
    let total = max(info.articleCount, 1)
    var done = 0
    while done < total {
      guard await pass(configuration.tick) else { return false }
      done = min(total, done + max(1, Int(configuration.articlesCheckedPerSecond * configuration.tick)))
      emitProgress(fraction: Double(done) / Double(total), filesDone: 0, filesTotal: info.files.count)
    }
    return true
  }

  /// Moves `total` bytes at the simulated speed, pausing when asked.
  private mutating func transfer(_ transferPhase: JobPhase, total: Int64, from start: Int64) async -> Bool {
    enter(transferPhase, emitsProgress: false)
    job.setTransferring(true)
    defer { job.setTransferring(false) }
    let files =
      transferPhase == .downloading
      ? info.files.filter { $0.kind != .par2 || settings.advanced.downloadAllRecoveryUpFront } : info.files.filter { $0.kind == .par2 }
    var done = min(start, total)
    var phaseTime: Double = 0
    emitTransfer(done: done, total: total, files: files)
    while done < total {
      if job.isStopped { return false }
      if job.isPaused {
        smoothedSpeed = 0
        emitTransfer(done: done, total: total, files: files, paused: true)
        while job.isPaused {
          if job.isStopped { return false }
          await sleep(configuration.tick)
        }
        engine.networkChanged()
        emitTransfer(done: done, total: total, files: files)
        continue
      }
      guard await pass(configuration.tick) else { return false }
      phaseTime += configuration.tick
      downloadSeconds += configuration.tick
      let speed = nextSpeed(phaseTime: phaseTime)
      let bytes = min(Int64(speed * configuration.tick), total - done)
      done += bytes
      wireBytes += Int64(Double(bytes) * 1.03)
      // An exponential average over about two seconds, as the engine smooths it.
      let alpha = min(1, configuration.tick / 2)
      smoothedSpeed = smoothedSpeed == 0 ? speed : smoothedSpeed + alpha * (speed - smoothedSpeed)
      if transferPhase == .downloading {
        sidecar.bytesDone = done
        if scenario.losesArticles {
          articlesFailed = Int64(Double(info.articleCount) * scenario.missingShare * Double(done) / Double(max(total, 1)))
        }
      }
      emitTransfer(done: done, total: total, files: files)
    }
    return true
  }

  private mutating func postProcess(passwords: [String], repairing: Bool = true) async -> JobSummary {
    let processing = settings.processing
    if repairing {
      if processing.repairWithPar2 && info.par2Bytes > 0 {
        guard await timed(.verifying, seconds: Double(info.dataBytes) / configuration.verifyBytesPerSecond, filesTotal: dataFiles.count) else {
          return stopped()
        }
        if scenario.losesArticles {
          let blocks = scenario == .repair ? 12 : 412
          guard await timed(.repairing, seconds: Double(blocks) * configuration.repairSecondsPerBlock, damagedBlocks: blocks) else { return stopped() }
          let repaired = scenario == .repair
          par2 = Par2Report(ran: true, verifiedOK: false, damagedBlocks: blocks, repairedBlocks: repaired ? blocks : 0, repaired: repaired)
        } else {
          par2 = Par2Report(ran: true, verifiedOK: true)
        }
      } else {
        par2 = Par2Report(skippedReason: processing.repairWithPar2 ? "The NZB has no recovery files." : "Repair is turned off in settings.")
      }
      if scenario == .failure {
        return summary(.failed, message: Self.missingSentence(scenario.missingShare))
      }
    }

    var extracted = 0
    var failedArchives = 0
    if processing.extractArchives && archiveSets > 0 {
      if scenario == .password && !passwords.contains(where: { !$0.isEmpty && !$0.lowercased().contains("wrong") }) {
        enter(.extracting)
        _ = await pass(configuration.tick)
        let message =
          request.passwords.isEmpty
          ? "The archive is encrypted. Enter its password to finish."
          : "The archive is encrypted and none of the passwords worked."
        return summary(.needsPassword, message: message, resumable: true)
      }
      guard await timed(.extracting, seconds: Double(info.dataBytes) / configuration.extractBytesPerSecond, filesTotal: archiveSets) else {
        return stopped()
      }
      failedArchives = scenario == .unrepairable ? 1 : 0
      extracted = archiveSets - failedArchives
    }

    var renamed = 0
    if processing.renameObfuscatedFiles && dataFiles.contains(where: { ReleaseName.looksObfuscated($0.name) || ReleaseName.looksObfuscated(stem($0.name)) }) {
      guard await timed(.renaming, seconds: configuration.renameSeconds) else { return stopped() }
      renamed = 1
    }

    let files = outputFiles(extracted: extracted > 0)
    writePlaceholders(files)
    if scenario == .unrepairable {
      return summary(
        .completedWithIssues, message: "Some files are incomplete because \(Self.percent(scenario.missingShare)) of articles are missing.",
        files: files, archivesExtracted: extracted, archivesFailed: failedArchives, filesRenamed: renamed)
    }
    return summary(.completed, files: files, archivesExtracted: extracted, filesRenamed: renamed)
  }

  /// A phase with nothing to transfer, ticking through its duration.
  private mutating func timed(_ timedPhase: JobPhase, seconds: Double, filesTotal: Int = 0, damagedBlocks: Int = 0) async -> Bool {
    enter(timedPhase, filesTotal: filesTotal, damagedBlocks: damagedBlocks)
    let total = max(seconds, configuration.tick)
    var time: Double = 0
    while time < total {
      guard await pass(configuration.tick) else { return false }
      time += configuration.tick
      let fraction = min(1, time / total)
      let filesDone = min(filesTotal, Int(Double(filesTotal) * fraction))
      emitProgress(fraction: fraction, filesDone: filesDone, filesTotal: filesTotal, damagedBlocks: damagedBlocks)
    }
    return true
  }

  // MARK: Clock and speed

  /// Lets `seconds` of simulated time go by, a tick at a time so a stop is
  /// noticed promptly. False when the job was stopped meanwhile.
  private mutating func pass(_ seconds: Double) async -> Bool {
    var remaining = seconds
    repeat {
      if job.isStopped { return false }
      let step = min(remaining, configuration.tick)
      await sleep(step)
      elapsed += step
      remaining -= step
    } while remaining > 0
    return !job.isStopped
  }

  private func sleep(_ simulatedSeconds: Double) async {
    let real = simulatedSeconds / max(configuration.timeScale, 0.001)
    if real >= 0.000_5 {
      try? await Task.sleep(for: .seconds(real))
    } else {
      await Task.yield()
    }
  }

  /// A random walk around the mean, a ramp over the first seconds while the
  /// connections open, and the engine-wide limit shared by every transfer.
  private mutating func nextSpeed(phaseTime: Double) -> Double {
    let noise = Double.random(in: -1...1, using: &generator)
    speedFactor += 0.25 * ((1 + configuration.jitter * noise) - speedFactor)
    var speed = configuration.meanBytesPerSecond * speedFactor * min(1, 0.3 + phaseTime / 2.5)
    if let limit = engine.speedLimit {
      speed = min(speed, Double(limit) / Double(max(engine.transferringJobCount, 1)))
    }
    return max(speed, 1)
  }

  // MARK: Events

  /// Announces a phase and its first progress. A transfer sends its own
  /// first progress (from where a resumed job left off), not a zero.
  private mutating func enter(_ newPhase: JobPhase, filesTotal: Int = 0, damagedBlocks: Int = 0, emitsProgress: Bool = true) {
    phase = newPhase
    job.setUsesNetwork(newPhase.usesNetwork)
    engine.networkChanged()
    job.emit(.phase(newPhase))
    if emitsProgress {
      emitProgress(fraction: 0, filesDone: 0, filesTotal: filesTotal, damagedBlocks: damagedBlocks)
    }
  }

  private func emitProgress(fraction: Double, filesDone: Int = 0, filesTotal: Int = 0, damagedBlocks: Int = 0) {
    let detail: String? = phase == .extracting && filesTotal > 1 ? "\(min(filesDone + 1, filesTotal)) of \(filesTotal)" : nil
    job.emit(
      .progress(
        JobProgress(
          phase: phase, filesDone: filesDone, filesTotal: filesTotal, articlesFailed: articlesFailed, fraction: fraction, detail: detail,
          damagedBlocks: damagedBlocks)))
  }

  private func emitTransfer(done: Int64, total: Int64, files: [NzbFile], paused: Bool = false) {
    var cumulative: Int64 = 0
    var filesDone = 0
    for file in files {
      cumulative += file.bytes
      if cumulative <= done { filesDone += 1 }
    }
    let speed = paused ? 0 : smoothedSpeed
    let eta: Int64? = speed > 0 ? Int64((Double(total - done) / speed).rounded(.up)) : nil
    job.emit(
      .progress(
        JobProgress(
          phase: phase, bytesDone: done, bytesTotal: total, speedBytesPerSecond: speed, etaSeconds: eta, filesDone: filesDone, filesTotal: files.count,
          articlesFailed: articlesFailed, fraction: total > 0 ? Double(done) / Double(total) : 1, paused: paused)))
  }

  // MARK: Results

  private func availabilityReport() -> AvailabilityInfo {
    let total = Int64(info.articleCount)
    // The scan underestimates a failing release: articles keep vanishing.
    let share: Double =
      switch scenario {
      case .repair: scenario.missingShare
      case .failure: 0.002
      case .unrepairable: scenario.missingShare
      default: 0
      }
    let missing = Int64(Double(total) * share)
    let verdict: AvailabilityInfo.Verdict =
      switch scenario {
      case .unrepairable: .unrepairable
      case .repair, .failure: .repairable
      default: info.par2Bytes > 0 || missing == 0 ? .complete : .unknown
      }
    return AvailabilityInfo(
      articlesTotal: total, articlesMissing: missing, missingBytes: Int64(Double(info.dataBytes) * share),
      recoveryBytes: info.par2Bytes, verdict: verdict)
  }

  private func stopped() -> JobSummary {
    saveSidecar()
    return summary(.stopped, resumable: true)
  }

  private func failure(_ error: EngineError) -> JobSummary {
    summary(.failed, message: error.message, errorKind: error.kind, resumable: true)
  }

  private func summary(
    _ outcome: Outcome, message: String? = nil, errorKind: EngineError.Kind? = nil, availability: AvailabilityInfo? = nil, files: [OutputFile] = [],
    archivesExtracted: Int = 0, archivesFailed: Int = 0, filesRenamed: Int = 0, resumable: Bool = false
  ) -> JobSummary {
    JobSummary(
      outcome: outcome, message: message, errorKind: errorKind, outputDirectory: request.outputDirectory, files: files,
      dataBytes: outcome.isSuccess ? decodedBytes : sidecar.bytesDone, wireBytes: wireBytes, elapsedSeconds: elapsed,
      downloadSeconds: downloadSeconds, articlesTotal: Int64(info.articleCount),
      articlesFailed: outcome == .failed && scenario == .failure ? Int64(Double(info.articleCount) * scenario.missingShare) : articlesFailed, par2: par2,
      archivesExtracted: archivesExtracted, archivesFailed: archivesFailed, filesRenamed: filesRenamed, availability: availability, resumable: resumable)
  }

  private func saveSidecar() {
    guard configuration.writesFiles else { return }
    sidecar.write(in: request.outputDirectory)
  }

  /// yEnc adds a few per cent; what lands on disk is that much smaller.
  private var decodedBytes: Int64 { Int64(Double(info.dataBytes) * 0.97) }

  private var dataFiles: [NzbFile] { info.files.filter { $0.kind != .par2 } }

  /// Archive sets: one per first volume (.part01.rar, .rar, .001, .7z, .zip).
  private var archiveSets: Int {
    let archives = info.files.filter { $0.kind == .archive }
    guard !archives.isEmpty else { return 0 }
    let firsts = archives.filter { file in
      let lower = file.name.lowercased()
      if lower.range(of: #"\.part0*1\.rar$"#, options: .regularExpression) != nil { return true }
      if lower.hasSuffix(".rar") { return lower.range(of: #"\.part\d+\.rar$"#, options: .regularExpression) == nil }
      return lower.hasSuffix(".001") || lower.hasSuffix(".7z") || lower.hasSuffix(".zip")
    }
    return max(firsts.count, 1)
  }

  /// What the folder holds afterwards: the extracted file under the release's
  /// name, and whatever the settings say to keep.
  private func outputFiles(extracted: Bool) -> [OutputFile] {
    var files: [OutputFile] = []
    if extracted {
      let ext: String? =
        switch info.contentKind {
        case .video: "mkv"
        case .audio: "flac"
        case .document: "epub"
        case .image: "jpg"
        case .software: "dmg"
        case .archive, .other: nil
        }
      let name = ext.map { "\(ReleaseName.folderName(info.title)).\($0)" } ?? ReleaseName.folderName(info.title)
      files.append(OutputFile(name: name, bytes: decodedBytes))
      if !settings.processing.deleteArchivesAfterExtracting {
        files += info.files.filter { $0.kind == .archive }.map { OutputFile(name: $0.name, bytes: Int64(Double($0.bytes) * 0.97)) }
      }
    } else {
      files += dataFiles.map { OutputFile(name: $0.name, bytes: Int64(Double($0.bytes) * 0.97)) }
    }
    if !settings.processing.deletePar2AfterRepairing {
      files += info.files.filter { $0.kind == .par2 }.map { OutputFile(name: $0.name, bytes: Int64(Double($0.bytes) * 0.97)) }
    }
    return files
  }

  private func writePlaceholders(_ files: [OutputFile]) {
    guard configuration.writesFiles else { return }
    let manager = FileManager.default
    for file in files {
      let url = request.outputDirectory.appending(path: ReleaseName.folderName(file.name))
      if !manager.fileExists(atPath: url.path(percentEncoded: false)) {
        manager.createFile(atPath: url.path(percentEncoded: false), contents: nil)
      }
    }
  }

  private func stem(_ name: String) -> String {
    var stem = name
    while !(stem as NSString).pathExtension.isEmpty, (stem as NSString).pathExtension.count <= 6 {
      stem = (stem as NSString).deletingPathExtension
    }
    return stem
  }

  static func percent(_ share: Double) -> String {
    share < 0.01 && share > 0 ? "less than 1%" : share.formatted(.percent.precision(.fractionLength(0)))
  }

  static func missingSentence(_ share: Double) -> String {
    let percent = percent(share)
    return "\(percent.prefix(1).uppercased() + percent.dropFirst()) of articles are missing and there is not enough recovery data."
  }
}
