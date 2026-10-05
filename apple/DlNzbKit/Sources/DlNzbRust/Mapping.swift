import DlNzbFFI
import DlNzbKit
import Foundation

// Conversions between DlNzbKit's Swift models and the generated FFI types.
// Most names exist in both modules, so every type here is module-qualified.
// Sizes and counts arrive as UInt64/UInt32 and leave as the Int64/Int the
// models use (clamped, never trapping).

// MARK: Settings to the engine

extension DlNzbFFI.ServerConfig {
  /// The server as the engine takes it. Throws `.config` for values the
  /// engine's integer types cannot hold; the engine checks the rest.
  init(_ server: ServerSettings, password: String) throws {
    guard let port = UInt16(exactly: server.port), port > 0 else {
      throw DlNzbKit.EngineError(.config, "The port must be between 1 and 65535.")
    }
    guard let connections = UInt16(exactly: server.connections), ServerSettings.connectionRange.contains(server.connections) else {
      throw DlNzbKit.EngineError(.config, "Connections must be between 1 and 100.")
    }
    self.init(
      host: server.normalisedHost,
      port: port,
      ssl: server.useSSL,
      verifyCertificate: server.verifyCertificate,
      username: server.username,
      password: password,
      connections: connections,
      retryAttempts: UInt8(clamping: server.retryAttempts))
  }
}

extension DlNzbFFI.EngineConfig {
  init(_ settings: EngineSettings) throws {
    self.init(
      server: try DlNzbFFI.ServerConfig(settings.server, password: settings.password),
      autoPar2Repair: settings.processing.repairWithPar2,
      autoExtractRar: settings.processing.extractArchives,
      deleteRarAfterExtract: settings.processing.deleteArchivesAfterExtracting,
      deletePar2AfterRepair: settings.processing.deletePar2AfterRepairing,
      deobfuscateFileNames: settings.processing.renameObfuscatedFiles,
      downloadAllPar2: settings.advanced.downloadAllRecoveryUpFront,
      fsyncOnFinalize: settings.advanced.flushFilesWhenFinished,
      speedLimitBytesPerSecond: settings.speedLimitBytesPerSecond.flatMap(UInt64.speedLimit))
  }
}

extension UInt64 {
  /// A speed limit for the engine: nil (unlimited) for nil, zero or less.
  static func speedLimit(_ bytesPerSecond: Int64) -> UInt64? {
    bytesPerSecond > 0 ? UInt64(bytesPerSecond) : nil
  }
}

extension DlNzbKit.ImportedSettings {
  /// The CLI's settings as the engine read them. The CLI's own preflight rule
  /// is the app's Automatic.
  init(_ imported: DlNzbFFI.ImportedConfig) {
    let config = imported.config
    let server = config.server
    self.init(
      server: ServerSettings(
        host: server.host,
        port: Int(server.port),
        useSSL: server.ssl,
        verifyCertificate: server.verifyCertificate,
        username: server.username,
        connections: Int(server.connections).clamped(to: ServerSettings.connectionRange),
        retryAttempts: Int(server.retryAttempts).clamped(to: ServerSettings.retryRange)),
      password: server.password,
      processing: ProcessingSettings(
        repairWithPar2: config.autoPar2Repair,
        extractArchives: config.autoExtractRar,
        deleteArchivesAfterExtracting: config.deleteRarAfterExtract,
        deletePar2AfterRepairing: config.deletePar2AfterRepair,
        renameObfuscatedFiles: config.deobfuscateFileNames),
      advanced: AdvancedSettings(
        preflight: .automatic,
        downloadAllRecoveryUpFront: config.downloadAllPar2,
        flushFilesWhenFinished: config.fsyncOnFinalize),
      downloadDirectory: imported.downloadDir.map { URL(filePath: Self.outsideContainer($0), directoryHint: .isDirectory) },
      source: URL(filePath: imported.source))
  }

  /// The engine expands `~` with `HOME`, which in the sandboxed Mac app is its
  /// container; the CLI meant the user's real home.
  static func outsideContainer(_ path: String, home: String = NSHomeDirectory()) -> String {
    #if os(macOS)
      var realHome = CLIConfig.realHomeDirectory.path(percentEncoded: false)
      while realHome.count > 1 && realHome.hasSuffix("/") { realHome.removeLast() }
      guard home != realHome, path == home || path.hasPrefix(home + "/") else { return path }
      return realHome + path.dropFirst(home.count)
    #else
      return path
    #endif
  }
}

// MARK: Requests

extension DlNzbFFI.Preflight {
  init(_ preflight: DlNzbKit.Preflight) {
    switch preflight {
    case .automatic: self = .auto
    case .always: self = .always
    case .never: self = .never
    }
  }
}

extension DlNzbFFI.OnUnrepairable {
  init(_ policy: DlNzbKit.OnUnrepairable) {
    switch policy {
    case .stop: self = .stop
    case .continue: self = .continue
    }
  }
}

extension DlNzbFFI.JobRequest {
  init(_ request: DlNzbKit.JobRequest, freeSpaceHint: UInt64?) {
    self.init(
      nzbPath: request.nzbURL.path(percentEncoded: false),
      outputDir: request.outputDirectory.path(percentEncoded: false),
      passwords: request.passwords,
      preflight: DlNzbFFI.Preflight(request.preflight),
      onUnrepairable: DlNzbFFI.OnUnrepairable(request.onUnrepairable),
      freeSpaceHint: freeSpaceHint,
      title: request.title)
  }
}

// MARK: Engine to models

extension DlNzbKit.JobEvent {
  init(_ event: DlNzbFFI.JobEvent) {
    switch event {
    case .phase(let phase): self = .phase(DlNzbKit.JobPhase(phase))
    case .progress(let progress): self = .progress(DlNzbKit.JobProgress(progress))
    case .availability(let info): self = .availability(DlNzbKit.AvailabilityInfo(info))
    case .warning(let message): self = .warning(message)
    case .finished(let summary): self = .finished(DlNzbKit.JobSummary(summary))
    }
  }
}

extension DlNzbKit.JobPhase {
  init(_ phase: DlNzbFFI.JobPhase) {
    switch phase {
    case .connecting: self = .connecting
    case .checking: self = .checking
    case .downloading: self = .downloading
    case .downloadingRecovery: self = .downloadingRecovery
    case .verifying: self = .verifying
    case .repairing: self = .repairing
    case .extracting: self = .extracting
    case .renaming: self = .renaming
    }
  }
}

extension DlNzbKit.JobProgress {
  init(_ progress: DlNzbFFI.JobProgress) {
    self.init(
      phase: DlNzbKit.JobPhase(progress.phase),
      bytesDone: Int64(clamping: progress.bytesDone),
      bytesTotal: Int64(clamping: progress.bytesTotal),
      speedBytesPerSecond: progress.speedBps,
      etaSeconds: progress.etaSecs.map { Int64(clamping: $0) },
      filesDone: Int(progress.filesDone),
      filesTotal: Int(progress.filesTotal),
      articlesFailed: Int64(clamping: progress.articlesFailed),
      fraction: progress.fraction,
      detail: progress.detail,
      paused: progress.paused,
      damagedBlocks: Int(clamping: progress.damagedBlocks))
  }
}

extension DlNzbKit.Outcome {
  init(_ outcome: DlNzbFFI.Outcome) {
    switch outcome {
    case .completed: self = .completed
    case .completedWithIssues: self = .completedWithIssues
    case .failed: self = .failed
    case .stopped: self = .stopped
    case .needsPassword: self = .needsPassword
    case .unrepairable: self = .unrepairable
    }
  }
}

extension DlNzbKit.EngineError.Kind {
  init(_ kind: DlNzbFFI.ErrorKind) {
    switch kind {
    case .config: self = .config
    case .auth: self = .auth
    case .dns: self = .dns
    case .connect: self = .connect
    case .tls: self = .tls
    case .timeout: self = .timeout
    case .protocol: self = .protocol
    case .nzb: self = .nzb
    case .io: self = .io
    case .diskFull: self = .diskFull
    }
  }
}

extension DlNzbKit.EngineError {
  /// The engine's error as the stores see it. Anything else that comes out of
  /// the bindings (a Rust panic turned into an error) is an `.io` failure.
  init(_ error: any Error) {
    switch error {
    case let error as DlNzbKit.EngineError:
      self = error
    case let error as DlNzbFFI.EngineError:
      switch error {
      case .Config(let message): self.init(.config, message)
      case .Auth(let message): self.init(.auth, message)
      case .Dns(let message): self.init(.dns, message)
      case .Connect(let message): self.init(.connect, message)
      case .Tls(let message): self.init(.tls, message)
      case .Timeout(let message): self.init(.timeout, message)
      case .Protocol(let message): self.init(.protocol, message)
      case .Nzb(let message): self.init(.nzb, message)
      case .Io(let message): self.init(.io, message)
      case .DiskFull(let message): self.init(.diskFull, message)
      }
    default:
      self.init(.io, "The engine failed unexpectedly (\(error.localizedDescription)).")
    }
  }
}

extension DlNzbKit.Par2Report {
  init(_ report: DlNzbFFI.Par2Report) {
    self.init(
      ran: report.ran,
      verifiedOK: report.verifiedOk,
      damagedBlocks: Int(clamping: report.damagedBlocks),
      repairedBlocks: Int(clamping: report.repairedBlocks),
      repaired: report.repaired,
      skippedReason: report.skippedReason)
  }
}

extension DlNzbKit.OutputFile {
  init(_ file: DlNzbFFI.OutputFile) {
    self.init(name: file.name, bytes: Int64(clamping: file.bytes))
  }
}

extension DlNzbKit.JobSummary {
  /// The models have no field yet for the engine's per-file reports and its
  /// check and post-processing times; they are dropped here.
  init(_ summary: DlNzbFFI.JobSummary) {
    self.init(
      outcome: DlNzbKit.Outcome(summary.outcome),
      message: summary.message,
      errorKind: summary.errorKind.map(DlNzbKit.EngineError.Kind.init),
      outputDirectory: URL(filePath: summary.outputDir, directoryHint: .isDirectory),
      files: summary.files.map(DlNzbKit.OutputFile.init),
      dataBytes: Int64(clamping: summary.dataBytes),
      wireBytes: Int64(clamping: summary.wireBytes),
      elapsedSeconds: summary.elapsedSecs,
      downloadSeconds: summary.downloadSecs,
      articlesTotal: Int64(clamping: summary.articlesTotal),
      articlesFailed: Int64(clamping: summary.articlesFailed),
      par2: DlNzbKit.Par2Report(summary.par2),
      archivesExtracted: Int(summary.archivesExtracted),
      archivesFailed: Int(summary.archivesFailed),
      filesRenamed: Int(summary.filesRenamed),
      availability: summary.availability.map(DlNzbKit.AvailabilityInfo.init),
      resumable: summary.resumable)
  }
}

extension DlNzbKit.AvailabilityInfo {
  init(_ info: DlNzbFFI.AvailabilityInfo) {
    self.init(
      articlesTotal: Int64(clamping: info.articlesTotal),
      articlesMissing: Int64(clamping: info.articlesMissing),
      missingBytes: Int64(clamping: info.missingBytes),
      recoveryBytes: Int64(clamping: info.recoveryBytes),
      verdict: Verdict(info.verdict))
  }
}

extension DlNzbKit.AvailabilityInfo.Verdict {
  init(_ verdict: DlNzbFFI.Verdict) {
    switch verdict {
    case .complete: self = .complete
    case .repairable: self = .repairable
    case .unrepairable: self = .unrepairable
    case .unknown: self = .unknown
    }
  }
}

extension DlNzbKit.NzbInfo {
  init(_ info: DlNzbFFI.NzbInfo) {
    self.init(
      title: info.title,
      passwords: info.passwords,
      category: info.category,
      totalBytes: Int64(clamping: info.totalBytes),
      dataBytes: Int64(clamping: info.dataBytes),
      par2Bytes: Int64(clamping: info.par2Bytes),
      files: info.files.map(DlNzbKit.NzbFile.init),
      contentKind: DlNzbKit.ContentKind(info.contentKind))
  }
}

extension DlNzbKit.NzbFile {
  init(_ file: DlNzbFFI.NzbFile) {
    self.init(name: file.name, bytes: Int64(clamping: file.bytes), segments: Int(file.segments), kind: Kind(file.kind))
  }
}

extension DlNzbKit.NzbFile.Kind {
  init(_ kind: DlNzbFFI.FileKind) {
    switch kind {
    case .data: self = .data
    case .par2: self = .par2
    case .archive: self = .archive
    case .other: self = .other
    }
  }
}

extension DlNzbKit.ContentKind {
  init(_ kind: DlNzbFFI.ContentKind) {
    switch kind {
    case .video: self = .video
    case .audio: self = .audio
    case .archive: self = .archive
    case .image: self = .image
    case .document: self = .document
    case .software: self = .software
    case .other: self = .other
    }
  }
}

extension DlNzbKit.ServerCheck {
  init(_ check: DlNzbFFI.ServerCheck) {
    self.init(greeting: check.greeting, tls: check.tls, latencyMilliseconds: Int(check.latencyMs))
  }
}

extension Comparable {
  fileprivate func clamped(to range: ClosedRange<Self>) -> Self {
    min(max(self, range.lowerBound), range.upperBound)
  }
}
