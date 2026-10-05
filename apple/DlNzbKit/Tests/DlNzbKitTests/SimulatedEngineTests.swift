import Foundation
import Testing

@testable import DlNzbKit

/// The stand-in engine: the phases it goes through, what it ends in for each
/// scenario, and that it honours pause, resume, stop and the speed limit.
@Suite("Simulated engine")
struct SimulatedEngineTests {
  /// Everything a session sends, in order, until it finishes.
  private func collect(_ session: JobSession) async -> [JobEvent] {
    var events: [JobEvent] = []
    for await event in session.events { events.append(event) }
    return events
  }

  private func phases(_ events: [JobEvent]) -> [JobPhase] {
    events.compactMap { if case .phase(let phase) = $0 { phase } else { nil } }
  }

  private func summary(_ events: [JobEvent]) -> JobSummary? {
    if case .finished(let summary) = events.last { return summary }
    return nil
  }

  private func request(_ title: String, in scratch: Scratch, preflight: Preflight = .automatic, onUnrepairable: OnUnrepairable = .stop) throws
    -> JobRequest
  {
    let nzb = try TestNZB.write(title: title, in: scratch.folder("Inbox"))
    return JobRequest(
      nzbURL: nzb, outputDirectory: scratch.url.appending(path: "Downloads/\(title)", directoryHint: .isDirectory), preflight: preflight,
      onUnrepairable: onUnrepairable)
  }

  @Test("A healthy job goes connect, check, download, verify, extract and finishes once, last")
  func normalJob() async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast())
    engine.scenarioOverride = .normal
    let events = await collect(try await engine.start(try request("Clean.Release.2024.1080p", in: scratch)))
    #expect(phases(events) == [.connecting, .checking, .downloading, .verifying, .extracting])
    #expect(events.filter { if case .finished = $0 { true } else { false } }.count == 1)
    let result = try #require(summary(events))
    #expect(result.outcome == .completed)
    #expect(result.par2.ran && result.par2.verifiedOK)
    #expect(result.archivesExtracted == 1)
    #expect(result.articlesTotal > 0 && result.articlesFailed == 0)
    #expect(!result.files.isEmpty)
    #expect(events.contains { if case .availability(let a) = $0 { a.verdict == .complete } else { false } })
  }

  @Test("Progress stays within its phase and bytes never go backwards")
  func progressIsOrderly() async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast())
    engine.scenarioOverride = .repair
    let events = await collect(try await engine.start(try request("Orderly.Release", in: scratch)))
    var phase: JobPhase?
    var lastBytes: Int64 = 0
    for event in events {
      switch event {
      case .phase(let next):
        phase = next
        lastBytes = 0
      case .progress(let progress):
        #expect(progress.phase == phase)
        #expect((0...1).contains(progress.fraction))
        #expect(progress.bytesDone >= lastBytes)
        #expect(progress.bytesDone <= max(progress.bytesTotal, 0))
        lastBytes = progress.bytesDone
      default:
        break
      }
    }
  }

  @Test(
    "Each scenario ends the way its name asks",
    arguments: [
      (SimulatedScenario.normal, Outcome.completed),
      (.repair, .completed),
      (.unrepairable, .unrepairable),
      (.password, .needsPassword),
      (.failure, .failed),
      (.diskFull, .failed),
      (.authFailure, .failed),
      (.unreachable, .failed),
    ])
  func scenarioOutcomes(scenario: SimulatedScenario, outcome: Outcome) async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast())
    engine.scenarioOverride = scenario
    let summary = try #require(self.summary(await collect(try await engine.start(try request("Scenario.\(scenario.rawValue)", in: scratch)))))
    #expect(summary.outcome == outcome)
    switch scenario {
    case .repair:
      #expect(summary.par2.repairedBlocks == 12 && summary.par2.repaired)
    case .unrepairable:
      #expect(summary.availability?.verdict == .unrepairable)
      #expect(summary.message?.hasSuffix("of articles are missing and there is not enough recovery data.") == true)
    case .diskFull:
      #expect(summary.errorKind == .diskFull)
    case .authFailure:
      #expect(summary.errorKind == .auth)
    case .unreachable:
      #expect(summary.errorKind == .connect)
    case .failure:
      #expect(summary.errorKind == nil)
      #expect(summary.missingFraction > 0.08)
    default:
      break
    }
  }

  @Test("Names pick scenarios, and the rest are spread by a stable hash")
  func scenarioHints() {
    #expect(SimulatedScenario(hint: "Show.S01E01.UNREPAIRABLE") == .unrepairable)
    #expect(SimulatedScenario(hint: "Archive.Password.Protected") == .password)
    #expect(SimulatedScenario(hint: "Big.DiskFull.Release") == .diskFull)
    #expect(SimulatedScenario(hint: "badlogin test") == .authFailure)
    #expect(SimulatedScenario(hint: "offline") == .unreachable)
    #expect(SimulatedScenario(hint: "Will.Fail.1080p") == .failure)
    #expect(SimulatedScenario(hint: "Needs.Repair.720p") == .repair)
    let name = PreviewData.sintelTitle
    #expect(SimulatedScenario(hint: name) == SimulatedScenario(hint: name))
    #expect([.normal, .repair].contains(SimulatedScenario(hint: name)))
  }

  @Test("Download Anyway skips the scan and finishes with issues")
  func downloadAnyway() async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast())
    let events = await collect(try await engine.start(try request("Show.Unrepairable", in: scratch, preflight: .never, onUnrepairable: .continue)))
    #expect(!phases(events).contains(.checking))
    #expect(phases(events).contains(.downloadingRecovery))
    #expect(summary(events)?.outcome == .completedWithIssues)
  }

  @Test("A password from the NZB or the request opens the archive, and reprocess finishes the job")
  func passwords() async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast())
    let plain = try request("Encrypted.Password.Release", in: scratch)
    #expect(summary(await collect(try await engine.start(plain)))?.outcome == .needsPassword)
    let wrong = try #require(summary(await collect(try await engine.reprocess(directory: plain.outputDirectory, passwords: ["wrong guess"]))))
    #expect(wrong.outcome == .needsPassword)
    let right = try #require(summary(await collect(try await engine.reprocess(directory: plain.outputDirectory, passwords: ["letmein"]))))
    #expect(right.outcome == .completed)

    var withPassword = try request("Encrypted.Password.Release.Two", in: scratch)
    withPassword.passwords = ["hunter2"]
    #expect(summary(await collect(try await engine.start(withPassword)))?.outcome == .completed)
  }

  @Test("Pause holds a download until resumed, and says so once")
  func pauseAndResume() async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast(timeScale: 250))
    engine.scenarioOverride = .normal
    await engine.setSpeedLimit(bytesPerSecond: 50_000)
    let session = try await engine.start(try request("Pausing.Release", in: scratch))
    var sawPaused = 0
    var resumed = false
    var outcome: Outcome?
    for await event in session.events {
      switch event {
      case .progress(let progress) where progress.phase == .downloading && progress.bytesDone > 0 && !resumed:
        if progress.paused {
          sawPaused += 1
          #expect(progress.speedBytesPerSecond == 0)
          #expect(engine.transferringJobCount == 0)
          resumed = true
          await engine.setSpeedLimit(bytesPerSecond: nil)
          session.resume()
        } else {
          session.pause()
        }
      case .finished(let summary):
        outcome = summary.outcome
      default:
        break
      }
    }
    #expect(sawPaused == 1)
    #expect(outcome == .completed)
  }

  @Test("Stop ends promptly and resumably, and starting again continues without a second scan")
  func stopAndContinue() async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast(timeScale: 250))
    engine.scenarioOverride = .normal
    await engine.setSpeedLimit(bytesPerSecond: 200_000)
    let request = try request("Stopping.Release", in: scratch)
    let first = try await engine.start(request)
    var stoppedAt: Int64 = 0
    var firstSummary: JobSummary?
    for await event in first.events {
      if case .progress(let progress) = event, progress.phase == .downloading, progress.bytesDone > 1_000_000, stoppedAt == 0 {
        stoppedAt = progress.bytesDone
        first.stop()
      }
      if case .finished(let summary) = event { firstSummary = summary }
    }
    #expect(firstSummary?.outcome == .stopped)
    #expect(firstSummary?.resumable == true)
    #expect(FileManager.default.fileExists(atPath: request.outputDirectory.appending(path: SimulatedSidecar.fileName).path(percentEncoded: false)))

    await engine.setSpeedLimit(bytesPerSecond: nil)
    let events = await collect(try await engine.start(request))
    #expect(!phases(events).contains(.checking))
    let firstDownload = events.compactMap { if case .progress(let p) = $0, p.phase == .downloading { p } else { nil } }.first
    #expect((firstDownload?.bytesDone ?? 0) >= stoppedAt)
    #expect(summary(events)?.outcome == .completed)
  }

  @Test("The speed limit caps every progress report")
  func speedLimit() async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast(timeScale: 2_000))
    engine.scenarioOverride = .normal
    await engine.setSpeedLimit(bytesPerSecond: 10_000_000)
    #expect(engine.speedLimit == 10_000_000)
    let events = await collect(try await engine.start(try request("Limited.Release", in: scratch)))
    let speeds = events.compactMap { if case .progress(let p) = $0, p.phase == .downloading { p.speedBytesPerSecond } else { nil } }
    #expect(!speeds.isEmpty)
    #expect(speeds.allSatisfy { $0 <= 10_000_001 })
    await engine.setSpeedLimit(bytesPerSecond: 0)
    #expect(engine.speedLimit == nil)
  }

  @Test("Test Connection says what went wrong")
  func testConnection() async throws {
    let engine = SimulatedEngine(configuration: .fast())
    let server = ServerSettings(host: "news.example.com", username: "zeph")
    let check = try await engine.testConnection(server, password: "secret")
    #expect(check.tls && check.latencyMilliseconds > 0)
    #expect(check.greeting.contains("news.example.com"))

    await #expect(throws: EngineError(.config, "Enter the server’s host name.")) {
      try await engine.testConnection(ServerSettings(host: " "), password: "")
    }
    let dns = await #expect(throws: EngineError.self) {
      try await engine.testConnection(ServerSettings(host: "news.example.invalid", username: "z"), password: "p")
    }
    #expect(dns?.kind == .dns)
    let auth = await #expect(throws: EngineError.self) { try await engine.testConnection(server, password: "wrong") }
    #expect(auth?.kind == .auth)
    engine.serverBehaviour = .unreachable
    let unreachable = await #expect(throws: EngineError.self) { try await engine.testConnection(server, password: "secret") }
    #expect(unreachable?.kind == .connect)
  }

  @Test("Settings with an impossible connection count are turned down")
  func applyValidates() async throws {
    let engine = SimulatedEngine(configuration: .fast())
    var settings = EngineSettings(server: ServerSettings(host: "news.example.com", connections: 0))
    await #expect(throws: EngineError.self) { try await engine.apply(settings) }
    settings.server.connections = 30
    settings.speedLimitBytesPerSecond = 5_000_000
    try await engine.apply(settings)
    #expect(engine.settings == settings)
    #expect(engine.speedLimit == 5_000_000)
  }

  @Test("Shutdown stops every job so it can continue")
  func shutdown() async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast(timeScale: 250))
    engine.scenarioOverride = .normal
    await engine.setSpeedLimit(bytesPerSecond: 50_000)
    let session = try await engine.start(try request("Shutdown.Release", in: scratch))
    let collector = Task { await collect(session) }
    try await Task.sleep(for: .milliseconds(30))
    await engine.shutdown()
    let events = await collector.value
    #expect(summary(events)?.outcome == .stopped)
    #expect(engine.runningJobCount == 0)
  }
}
