import DlNzbKit
import Foundation
import Testing

@testable import DlNzbRust

/// RustEngine against the real Rust engine, without a news server: parsing,
/// the errors a server that is not there produces, and a job's whole life.
@Suite("Rust engine")
struct RustEngineTests {
  /// Everything a session sends until its stream ends.
  private func collect(_ session: JobSession) async -> [DlNzbKit.JobEvent] {
    var events: [DlNzbKit.JobEvent] = []
    for await event in session.events { events.append(event) }
    return events
  }

  private func settings(port: Int) -> EngineSettings {
    EngineSettings(
      server: ServerSettings(host: "127.0.0.1", port: port, useSSL: false, username: "tester", connections: 2, retryAttempts: 0), password: "secret")
  }

  @Test("inspect reads an NZB without the network")
  func inspect() async throws {
    let info = try await RustEngine().inspect(Fixture.syntheticNZB, fileName: nil)
    #expect(info.title == "Example.Show.S01E02.1080p.WEB.x265-TEST")
    #expect(info.passwords == ["hunter2"])
    #expect(info.category == "TV")
    #expect(info.totalBytes == 1_322_000)
    #expect(info.par2Bytes == 40_000)
    #expect(info.dataBytes == 1_282_000)
    #expect(info.files.map(\.name) == ["example.show.s01e02.mkv", "example.show.s01e02.nfo", "example.show.s01e02.par2"])
    #expect(info.files.map(\.kind) == [.data, .other, .par2])
    #expect(info.articleCount == 4)
    #expect(info.contentKind == .video)
  }

  @Test("inspect says what is wrong with a file that is not an NZB")
  func inspectFailures() async throws {
    let scratch = try ScratchFolder()
    let junk = scratch.url.appending(path: "junk.nzb")
    try Data("not xml at all".utf8).write(to: junk)
    let engine = RustEngine()
    await #expect { try await engine.inspect(junk, fileName: nil) } throws: { ($0 as? EngineError)?.kind == .nzb }
    await #expect { try await engine.inspect(scratch.url.appending(path: "missing.nzb"), fileName: nil) } throws: { ($0 as? EngineError)?.kind == .io }
  }

  @Test("inspect reads the queue's copy as the file the user opened: its title and password")
  func inspectAsOpened() async throws {
    let scratch = try ScratchFolder()
    let copy = scratch.url.appending(path: "\(UUID().uuidString).nzb")
    let text = try String(contentsOf: Fixture.syntheticNZB, encoding: .utf8)
      .replacingOccurrences(of: #"<meta type="title">Example.Show.S01E02.1080p.WEB.x265-TEST</meta>"#, with: "")
    try Data(text.utf8).write(to: copy)
    let info = try await RustEngine().inspect(copy, fileName: "Example Show{{s3cret}}.nzb")
    #expect(info.title == "Example Show")
    #expect(info.passwords == ["hunter2", "s3cret"])
  }

  @Test("Test Connection to a port nobody listens on is a connection error")
  func refusedConnection() async throws {
    let server = settings(port: unusedLocalPort()).server
    await #expect {
      try await RustEngine().testConnection(server, password: "secret")
    } throws: { error in
      let error = error as? EngineError
      return error?.kind == .connect && error?.message.isEmpty == false
    }
  }

  @Test("Test Connection with no host asks for one")
  func noHost() async {
    await #expect { try await RustEngine().testConnection(ServerSettings(host: "  "), password: "") } throws: { ($0 as? EngineError)?.kind == .config }
  }

  @Test("apply turns down settings the engine cannot use")
  func applyValidates() async throws {
    let engine = RustEngine()
    try await engine.apply(settings(port: 119))
    var bad = settings(port: 119)
    bad.server.connections = 0
    await #expect { try await engine.apply(bad) } throws: { ($0 as? EngineError)?.kind == .config }
    await engine.setSpeedLimit(bytesPerSecond: 1_000_000)
    await engine.setSpeedLimit(bytesPerSecond: nil)
  }

  @Test("a job with an unreachable server finishes as failed, with the reason")
  func unreachableJob() async throws {
    let scratch = try ScratchFolder()
    let engine = RustEngine()
    try await engine.apply(settings(port: unusedLocalPort()))
    let output = scratch.url.appending(path: "Example Show", directoryHint: .isDirectory)
    let session = try await engine.start(JobRequest(nzbURL: try Fixture.syntheticNZB, outputDirectory: output))

    let events = await collect(session)
    guard case .finished(let summary) = events.last else {
      Issue.record("the last event was not .finished: \(events)")
      return
    }
    #expect(events.filter { if case .finished = $0 { true } else { false } }.count == 1)
    #expect(events.contains(.phase(.connecting)))
    #expect(summary.outcome == .failed)
    #expect(summary.errorKind == .connect)
    #expect(summary.message?.isEmpty == false)
    #expect(summary.outputDirectory.standardizedFileURL.path(percentEncoded: false).hasSuffix("Example Show/"))

    // The controls after the end do nothing, and do not crash.
    session.pause()
    session.resume()
    session.stop()
    await engine.shutdown()
  }

  @Test("stopping a job that is connecting ends it as stopped")
  func stopWhileConnecting() async throws {
    let scratch = try ScratchFolder()
    // Listening but never answering keeps the job in Connecting.
    let silent = try #require(boundLocalSocket())
    defer { close(silent.fd) }
    try #require(listen(silent.fd, 4) == 0)

    let engine = RustEngine()
    try await engine.apply(settings(port: silent.port))
    let session = try await engine.start(
      JobRequest(nzbURL: try Fixture.syntheticNZB, outputDirectory: scratch.url.appending(path: "Job", directoryHint: .isDirectory), preflight: .never))
    var events: [DlNzbKit.JobEvent] = []
    for await event in session.events {
      events.append(event)
      if event == .phase(.connecting) { session.stop() }
    }
    guard case .finished(let summary) = events.last else {
      Issue.record("no .finished: \(events)")
      return
    }
    #expect(summary.outcome == .stopped)
    // Nothing reached the disk before the stop, so there is no resume record:
    // starting it again is a fresh job (CONTRACT §1, `resumable`).
    #expect(!summary.resumable)
  }

  @Test("the free-space figure comes from the volume the job folder will be on")
  func freeSpace() throws {
    let scratch = try ScratchFolder()
    let notYet = scratch.url.appending(path: "Later/Job", directoryHint: .isDirectory)
    #expect((RustEngine.importantUsageCapacity(of: notYet) ?? 0) > 0)
    #if os(macOS)
      #expect(RustEngine.freeSpaceHint(for: notYet) == nil)
    #endif
  }

  @Test("the CLI's config file is read with the CLI's own parser")
  func importCLIConfig() async throws {
    let scratch = try ScratchFolder()
    let file = scratch.url.appending(path: "config.toml")
    try Data(
      """
      [usenet]
      server = "news.example.com"
      port = 443
      username = "zeph"
      password = "p\\"w#d"
      ssl = true
      verify_ssl_certs = false
      connections = 40
      timeout = 30
      retry_attempts = 3
      retry_delay = 500

      [download]
      dir = "/Volumes/Media/Usenet"
      create_subfolders = true

      [post_processing]
      auto_par2_repair = true
      auto_extract_rar = false
      delete_rar_after_extract = true
      delete_par2_after_repair = false
      deobfuscate_file_names = true
      download_all_par2 = true
      """.utf8
    ).write(to: file)

    let engine = RustEngine()
    let imported = try await engine.importCLIConfig(from: file)
    #expect(imported.server.host == "news.example.com")
    #expect(imported.server.port == 443)
    #expect(imported.server.connections == 40)
    #expect(imported.server.retryAttempts == 3)
    #expect(!imported.server.verifyCertificate)
    #expect(imported.password == "p\"w#d")
    #expect(!imported.processing.extractArchives)
    #expect(imported.processing.deleteArchivesAfterExtracting)
    #expect(imported.advanced.downloadAllRecoveryUpFront)

    // Through the protocol too, and with TOML only the real parser knows (a
    // \u escape), so the app's import is the CLI's reading of the file.
    let text = try String(contentsOf: file, encoding: .utf8).replacingOccurrences(of: #"password = "p\"w#d""#, with: #"password = "a\u0042c""#)
    try Data(text.utf8).write(to: file)
    let anyEngine: any DownloadEngine = engine
    #expect(try await anyEngine.importCLIConfig(from: file).password == "aBc")

    await #expect { try await engine.importCLIConfig(from: scratch.url.appending(path: "missing.toml")) } throws: { ($0 as? EngineError)?.kind == .io }
    try Data("[usenet]\nserver = \"\"\n".utf8).write(to: file)
    await #expect { try await engine.importCLIConfig(from: file) } throws: { ($0 as? EngineError)?.kind == .config }
  }
}
