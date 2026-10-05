import DlNzbKit
import Foundation
import Testing

@testable import DlNzbRust

/// A real download through RustEngine, with the settings of the dl-nzb CLI on
/// this Mac. Off unless asked for, as it needs a news server account:
///
///     DLNZB_LIVE=1 swift test --filter LiveTests
///
/// `DLNZB_LIVE_NZB` picks the NZB (default ~/Downloads/test.nzb) and
/// `DLNZB_LIVE_OUT` the folder the job folder goes in (default
/// ~/Downloads/dl-nzb-qa/ffi). The password is never printed.
@Suite("Live download", .enabled(if: ProcessInfo.processInfo.environment["DLNZB_LIVE"] == "1"))
struct LiveTests {
  @Test("downloads an NZB with the CLI's server and finishes completed")
  func download() async throws {
    let environment = ProcessInfo.processInfo.environment
    let home = URL(filePath: NSHomeDirectory(), directoryHint: .isDirectory)
    let nzb = environment["DLNZB_LIVE_NZB"].map { URL(filePath: $0) } ?? home.appending(path: "Downloads/test.nzb")
    let base = environment["DLNZB_LIVE_OUT"].map { URL(filePath: $0, directoryHint: .isDirectory) } ?? home.appending(path: "Downloads/dl-nzb-qa/ffi")

    let engine = RustEngine()
    let config = home.appending(path: "Library/Application Support/dl-nzb/config.toml")
    let imported = try await engine.importCLIConfig(from: config)
    try await engine.apply(
      EngineSettings(server: imported.server, password: imported.password, processing: imported.processing, advanced: imported.advanced))
    print("live: server \(imported.server.host):\(imported.server.port) ssl=\(imported.server.useSSL) connections=\(imported.server.connections)")

    let info = try await engine.inspect(nzb, fileName: nil)
    let output = base.appending(path: ReleaseName.folderName(info.title), directoryHint: .isDirectory)
    print("live: \(info.title), \(info.files.count) files, \(info.totalBytes) bytes -> \(output.path(percentEncoded: false))")

    let started = Date()
    let session = try await engine.start(JobRequest(nzbURL: nzb, outputDirectory: output))
    var summary: JobSummary?
    var lastPrinted = Date.distantPast
    for await event in session.events {
      let t = String(format: "%6.1fs", Date().timeIntervalSince(started))
      switch event {
      case .phase(let phase):
        print("live: \(t) phase \(phase.rawValue)")
      case .progress(let progress) where Date().timeIntervalSince(lastPrinted) >= 2:
        lastPrinted = Date()
        let speed = progress.speedBytesPerSecond / 1_000_000
        print(
          "live: \(t)   \(progress.phase.rawValue) \(Int(progress.displayFraction * 100))% \(progress.bytesDone)/\(progress.bytesTotal) "
            + String(format: "%.1f MB/s", speed) + " failed=\(progress.articlesFailed)" + (progress.detail.map { " \($0)" } ?? ""))
      case .progress:
        break
      case .availability(let availability):
        print("live: \(t) availability \(availability.verdict.rawValue), \(availability.articlesMissing) of \(availability.articlesTotal) missing")
      case .warning(let warning):
        print("live: \(t) warning: \(warning)")
      case .finished(let finished):
        summary = finished
        let average = finished.averageSpeed.map { String(format: "%.1f MB/s", $0 / 1_000_000) } ?? "-"
        print(
          "live: \(t) finished \(finished.outcome.rawValue) \(finished.message ?? "") files=\(finished.files.count) data=\(finished.dataBytes) "
            + "articles=\(finished.articlesTotal) failed=\(finished.articlesFailed) par2(ran=\(finished.par2.ran) ok=\(finished.par2.verifiedOK)) "
            + "extracted=\(finished.archivesExtracted) renamed=\(finished.filesRenamed) average=\(average)")
        for file in finished.files { print("live:          \(file.name) \(file.bytes)") }
      }
    }
    await engine.shutdown()
    let finished = try #require(summary)
    #expect(finished.outcome == .completed, "\(finished.message ?? "")")
    #expect(!finished.files.isEmpty)
  }
}
