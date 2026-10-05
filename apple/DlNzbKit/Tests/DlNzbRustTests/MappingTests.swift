import DlNzbFFI
import DlNzbKit
import Foundation
import Testing

@testable import DlNzbRust

/// The adapter's conversions between the generated bindings and the models.
@Suite("FFI mapping")
struct MappingTests {
  @Test("every engine enum maps to the model case of the same name")
  func enumsMapByName() {
    for phase in DlNzbFFI.JobPhase.allCases {
      #expect(DlNzbKit.JobPhase(phase).rawValue == "\(phase)")
    }
    #expect(Set(DlNzbFFI.JobPhase.allCases.map(DlNzbKit.JobPhase.init)) == Set(DlNzbKit.JobPhase.allCases))
    for outcome in DlNzbFFI.Outcome.allCases {
      #expect(DlNzbKit.Outcome(outcome).rawValue == "\(outcome)")
    }
    for kind in DlNzbFFI.ErrorKind.allCases {
      #expect(DlNzbKit.EngineError.Kind(kind).rawValue == "\(kind)")
    }
    #expect(Set(DlNzbFFI.ErrorKind.allCases.map(DlNzbKit.EngineError.Kind.init)) == Set(DlNzbKit.EngineError.Kind.allCases))
    for verdict in DlNzbFFI.Verdict.allCases {
      #expect(DlNzbKit.AvailabilityInfo.Verdict(verdict).rawValue == "\(verdict)")
    }
    for kind in DlNzbFFI.ContentKind.allCases {
      #expect(DlNzbKit.ContentKind(kind).rawValue == "\(kind)")
    }
    #expect(Set(DlNzbFFI.ContentKind.allCases.map(DlNzbKit.ContentKind.init)) == Set(DlNzbKit.ContentKind.allCases))
    for kind in DlNzbFFI.FileKind.allCases {
      #expect(DlNzbKit.NzbFile.Kind(kind).rawValue == "\(kind)")
    }
  }

  @Test("request policies map to the engine's")
  func policies() {
    #expect(DlNzbKit.Preflight.allCases.map(DlNzbFFI.Preflight.init) == [.auto, .always, .never])
    #expect(DlNzbFFI.OnUnrepairable(.stop) == .stop)
    #expect(DlNzbFFI.OnUnrepairable(.continue) == .continue)
  }

  @Test("a job request crosses as paths, with the free-space hint")
  func jobRequest() {
    let request = DlNzbKit.JobRequest(
      nzbURL: URL(filePath: "/tmp/In Box/a b.nzb"), outputDirectory: URL(filePath: "/tmp/Out/Some Show", directoryHint: .isDirectory),
      passwords: ["one", "two"], preflight: .never, onUnrepairable: .continue)
    let ffi = DlNzbFFI.JobRequest(request, freeSpaceHint: 42)
    #expect(ffi.nzbPath == "/tmp/In Box/a b.nzb")
    #expect(ffi.outputDir.hasPrefix("/tmp/Out/Some Show"))
    #expect(ffi.passwords == ["one", "two"])
    #expect(ffi.preflight == .never)
    #expect(ffi.onUnrepairable == .continue)
    #expect(ffi.freeSpaceHint == 42)
  }

  @Test("settings become the engine's config and come back as an import")
  func settingsRoundTrip() throws {
    let settings = EngineSettings(
      server: ServerSettings(
        host: " nntps://news.example.com/ ", port: 443, useSSL: true, verifyCertificate: false, username: "zeph", connections: 42, retryAttempts: 5),
      password: "p@ss word",
      processing: ProcessingSettings(
        repairWithPar2: false, extractArchives: true, deleteArchivesAfterExtracting: true, deletePar2AfterRepairing: true, renameObfuscatedFiles: false),
      advanced: AdvancedSettings(preflight: .automatic, downloadAllRecoveryUpFront: true, flushFilesWhenFinished: true),
      speedLimitBytesPerSecond: 5_000_000)
    let config = try DlNzbFFI.EngineConfig(settings)
    #expect(config.server.host == "news.example.com")
    #expect(config.server.password == "p@ss word")
    #expect(config.speedLimitBytesPerSecond == 5_000_000)

    let imported = DlNzbKit.ImportedSettings(DlNzbFFI.ImportedConfig(config: config, downloadDir: "/Volumes/Media", source: "/tmp/config.toml"))
    var expectedServer = settings.server
    expectedServer.host = "news.example.com"
    #expect(imported.server == expectedServer)
    #expect(imported.password == settings.password)
    #expect(imported.processing == settings.processing)
    #expect(imported.advanced == settings.advanced)
    #expect(imported.downloadDirectory?.path(percentEncoded: false).hasPrefix("/Volumes/Media") == true)
    #expect(imported.source == URL(filePath: "/tmp/config.toml"))
  }

  #if os(macOS)
    @Test("a CLI folder under the sandbox's home goes back to the real home")
    func importedFolderLeavesTheContainer() {
      let real = CLIConfig.realHomeDirectory.path(percentEncoded: false)
      let container = "/Users/someone/Library/Containers/com.zephleggett.dl-nzb/Data"
      let rebased = DlNzbKit.ImportedSettings.outsideContainer(container + "/Downloads/Usenet", home: container)
      #expect(rebased.hasSuffix("/Downloads/Usenet"))
      #expect(rebased.hasPrefix(real.hasSuffix("/") ? String(real.dropLast()) : real))
      #expect(DlNzbKit.ImportedSettings.outsideContainer("/Volumes/Media", home: container) == "/Volumes/Media")
    }
  #endif

  @Test("values the engine cannot hold are a settings problem")
  func invalidSettings() {
    for server in [ServerSettings(host: "h", port: 0), ServerSettings(host: "h", port: 70_000), ServerSettings(host: "h", connections: 0)] {
      #expect {
        _ = try DlNzbFFI.ServerConfig(server, password: "")
      } throws: { ($0 as? DlNzbKit.EngineError)?.kind == .config }
    }
    #expect(UInt64.speedLimit(0) == nil)
    #expect(UInt64.speedLimit(-5) == nil)
    #expect(UInt64.speedLimit(1) == 1)
  }

  @Test("events keep their payloads")
  func events() {
    let progress = DlNzbFFI.JobProgress(
      phase: .repairing, bytesDone: 10, bytesTotal: 20, speedBps: 3.5, etaSecs: 7, filesDone: 1, filesTotal: 4, articlesFailed: 9,
      fraction: 0.5, detail: "2 of 5", paused: true, damagedBlocks: 12)
    let expected = DlNzbKit.JobProgress(
      phase: .repairing, bytesDone: 10, bytesTotal: 20, speedBytesPerSecond: 3.5, etaSeconds: 7, filesDone: 1, filesTotal: 4, articlesFailed: 9,
      fraction: 0.5, detail: "2 of 5", paused: true, damagedBlocks: 12)
    #expect(DlNzbKit.JobEvent(.progress(progress: progress)) == .progress(expected))
    #expect(DlNzbKit.JobEvent(.phase(phase: .downloadingRecovery)) == .phase(.downloadingRecovery))
    #expect(DlNzbKit.JobEvent(.warning(message: "Heads up.")) == .warning("Heads up."))

    let availability = DlNzbFFI.AvailabilityInfo(articlesTotal: 100, articlesMissing: 3, missingBytes: 3000, recoveryBytes: 9000, verdict: .repairable)
    #expect(
      DlNzbKit.JobEvent(.availability(info: availability))
        == .availability(DlNzbKit.AvailabilityInfo(articlesTotal: 100, articlesMissing: 3, missingBytes: 3000, recoveryBytes: 9000, verdict: .repairable)))

    // Sizes beyond Int64 clamp instead of trapping.
    let huge = DlNzbFFI.OutputFile(name: "a", bytes: .max)
    #expect(DlNzbKit.OutputFile(huge).bytes == .max)
  }

  @Test("a finished summary maps every field the models have")
  func summary() {
    let summary = DlNzbFFI.JobSummary(
      outcome: .failed, message: "Could not connect.", errorKind: .connect, outputDir: "/tmp/Out/Show", files: [DlNzbFFI.OutputFile(name: "a.mkv", bytes: 5)],
      nzbFiles: [], dataBytes: 5, wireBytes: 6, elapsedSecs: 1.5, downloadSecs: 1.0, checkSecs: 0.25, postSecs: 0.125, articlesTotal: 10,
      articlesFailed: 2, par2: DlNzbFFI.Par2Report(ran: true, verifiedOk: true, damagedBlocks: 4, repairedBlocks: 4, repaired: true, skippedReason: nil),
      archivesExtracted: 1, archivesFailed: 0, filesRenamed: 2, availability: nil, resumable: true)
    let mapped = DlNzbKit.JobSummary(summary)
    #expect(mapped.outcome == .failed)
    #expect(mapped.errorKind == .connect)
    #expect(mapped.message == "Could not connect.")
    #expect(mapped.outputDirectory.path(percentEncoded: false).hasPrefix("/tmp/Out/Show"))
    #expect(mapped.files == [DlNzbKit.OutputFile(name: "a.mkv", bytes: 5)])
    #expect(mapped.par2 == DlNzbKit.Par2Report(ran: true, verifiedOK: true, damagedBlocks: 4, repairedBlocks: 4, repaired: true))
    #expect(mapped.articlesFailed == 2)
    #expect(mapped.filesRenamed == 2)
    #expect(mapped.resumable)
    #expect(DlNzbKit.JobEvent(.finished(summary: summary)) == .finished(mapped))
  }

  @Test("engine errors keep their kind and sentence")
  func errors() {
    let cases: [(DlNzbFFI.EngineError, DlNzbKit.EngineError.Kind)] = [
      (.Config(message: "m"), .config), (.Auth(message: "m"), .auth), (.Dns(message: "m"), .dns), (.Connect(message: "m"), .connect),
      (.Tls(message: "m"), .tls), (.Timeout(message: "m"), .timeout), (.Protocol(message: "m"), .protocol), (.Nzb(message: "m"), .nzb),
      (.Io(message: "m"), .io), (.DiskFull(message: "m"), .diskFull),
    ]
    for (ffi, kind) in cases {
      #expect(DlNzbKit.EngineError(ffi) == DlNzbKit.EngineError(kind, "m"))
    }
    #expect(DlNzbKit.EngineError(CocoaError(.fileReadUnknown)).kind == .io)
  }
}
