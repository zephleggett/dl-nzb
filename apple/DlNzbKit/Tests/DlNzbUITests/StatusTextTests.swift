import DlNzbKit
import Foundation
import Testing

@testable import DlNzbUI

/// The words both apps show: the status line for every state, per the SPEC's
/// lifecycle table, and the formatters under it. Pinned to US English.
@Suite("Status text")
struct StatusTextTests {
  let locale = Locale(identifier: "en_US")

  private func line(_ item: DownloadItem) -> String {
    StatusText.line(for: item, locale: locale)
  }

  @Test(
    "Every state has the SPEC's status line",
    arguments: [
      (PreviewData.queued, "Waiting · 8.2 GB"),
      (PreviewData.connecting, "Connecting…"),
      (PreviewData.checking, "Checking 11,090 articles…"),
      (PreviewData.downloading, "3.1 GB of 8.2 GB · 84 MB/s · 1 min left"),
      (PreviewData.paused, "Paused · 1.9 GB of 4.6 GB"),
      (PreviewData.downloadingRecovery, "Downloading recovery data · 118 MB of 336 MB"),
      (PreviewData.verifying, "Verifying · 43%"),
      (PreviewData.repairing, "Repairing 12 damaged blocks · 43%"),
      (PreviewData.extracting, "Extracting · 2 of 5"),
      (PreviewData.renaming, "Renaming files"),
      (PreviewData.finished, "8.2 GB · Finished in 3 min · Repaired 12 blocks"),
      (PreviewData.finishedWithIssues, "1 GB · Finished in 41 sec · 1 archive could not be extracted"),
      (PreviewData.failed, "9% of articles missing"),
      (PreviewData.needsAttentionUnrepairable, "9% of articles are missing and there is not enough recovery data"),
      (PreviewData.needsPassword, "The archive is encrypted"),
      (PreviewData.needsSpace, "This download needs 3.27 GB more free space"),
      (PreviewData.stopped, "Stopped · 412 MB of 1.0 GB"),
    ])
  func everyState(item: DownloadItem, expected: String) {
    #expect(line(item) == expected)
  }

  @Test("PreviewData covers every state the table names")
  func coverage() {
    #expect(PreviewData.allStates.count == 17)
    let phases = Set(PreviewData.allStates.compactMap(\.phase))
    #expect(phases == Set(JobPhase.allCases))
  }

  @Test("Status lines never end in a full stop, never say Loading, and separate with middle dots")
  func copyRules() {
    for item in PreviewData.allStates {
      let text = line(item)
      #expect(!text.hasSuffix("."), "\(text)")
      #expect(text.range(of: #"\bloading\b"#, options: [.regularExpression, .caseInsensitive]) == nil)
      #expect(!text.contains(" - ") && !text.contains("—"))
    }
  }

  @Test("Lines adapt to what is known")
  func edges() {
    var item = PreviewData.downloading
    item.progress?.etaSeconds = nil
    #expect(line(item) == "3.1 GB of 8.2 GB · 84 MB/s")
    item.progress?.speedBytesPerSecond = 0
    #expect(line(item) == "3.1 GB of 8.2 GB · Waiting for the server…")
    item.progress = nil
    #expect(line(item) == "Downloading · 8.2 GB")

    var repairing = PreviewData.repairing
    repairing.progress?.damagedBlocks = 1
    #expect(line(repairing) == "Repairing 1 damaged block · 43%")
    repairing.progress?.damagedBlocks = 0
    #expect(line(repairing) == "Repairing · 43%")

    var extracting = PreviewData.extracting
    extracting.progress?.filesTotal = 1
    #expect(line(extracting) == "Extracting · 31%")

    var password = PreviewData.needsPassword
    password.passwords = ["a guess"]
    #expect(line(password) == "That password did not work")
    #expect(line(password) == StatusText.passwordRejected)

    var unknownSize = PreviewData.queued
    unknownSize.info = nil
    #expect(line(unknownSize) == "Waiting")

    var serverFailure = PreviewData.failed
    serverFailure.summary?.errorKind = .io
    serverFailure.state = .failed(message: "dl-nzb could not write to the download folder.", resumable: true)
    #expect(line(serverFailure) == "dl-nzb could not write to the download folder")

    var rare = PreviewData.needsAttentionUnrepairable
    rare.state = .needsAttention(
      .unrepairable(AvailabilityInfo(articlesTotal: 10_000, articlesMissing: 3, missingBytes: 0, recoveryBytes: 0, verdict: .unrepairable)))
    #expect(line(rare) == "Less than 1% of articles are missing and there is not enough recovery data")

    var stoppedEarly = PreviewData.stopped
    stoppedEarly.progress = nil
    #expect(line(stoppedEarly) == "Stopped")

    var damaged = PreviewData.finishedWithIssues
    var summary = PreviewData.finishedWithIssuesSummary
    summary.archivesFailed = 0
    damaged.state = .finished(summary)
    #expect(line(damaged) == "1 GB · Finished in 41 sec · Some files damaged")
    summary.archivesFailed = 2
    damaged.state = .finished(summary)
    #expect(line(damaged).hasSuffix(" · 2 archives could not be extracted"))
  }

  @Test("Stopped reads Stopped, never Cancelled, wherever it shows")
  func stoppedCopy() {
    for item in PreviewData.allStates {
      #expect(!line(item).localizedCaseInsensitiveContains("cancel"), "\(line(item))")
    }
    #expect(line(PreviewData.stopped).hasPrefix("Stopped"))
    #expect(StatusSymbol.of(PreviewData.stopped)?.name == "stop.circle.fill")
  }

  @Test("Retry is Download Again when the item would start over")
  func retryTitles() {
    #expect(StatusText.retryTitle(for: PreviewData.failed) == "Download Again")
    var resumable = PreviewData.failed
    resumable.state = .failed(message: "The download stopped unexpectedly.", resumable: true)
    #expect(StatusText.retryTitle(for: resumable) == "Retry")
    #expect(StatusText.retryTitle(for: PreviewData.stopped) == "Retry")
    var deleted = PreviewData.stopped
    deleted.progress = nil
    #expect(StatusText.retryTitle(for: deleted) == "Download Again")
    #expect(StatusText.retryTitle(for: PreviewData.needsSpace) == "Retry")
  }

  @Test("A waiting item the queue holds back reads Paused")
  func heldLine() {
    #expect(StatusText.line(for: PreviewData.queued, held: true, locale: locale) == "Paused · 8.2 GB")
    #expect(StatusText.line(for: PreviewData.queued, held: false, locale: locale) == "Waiting · 8.2 GB")
    // Held only changes waiting items.
    #expect(StatusText.line(for: PreviewData.downloading, held: true, locale: locale) == line(PreviewData.downloading))
  }

  @Test("Waiting, downloading and finished show the same size for a release: its data, not its recovery files")
  func sizeConsistency() throws {
    let info = PreviewData.tearsOfSteelInfo
    #expect(info.totalBytes > info.dataBytes)
    var item = PreviewData.item(30, info, state: .queued)
    let size = Format.bytes(info.dataBytes, locale: locale)
    #expect(line(item) == "Waiting · \(size)")
    item.state = .running(.downloading)
    item.progress = JobProgress(phase: .downloading, bytesDone: 1_000_000_000, bytesTotal: info.dataBytes, speedBytesPerSecond: 50_000_000)
    #expect(line(item).hasPrefix("1.0 GB of \(size)"))
    let summary = JobSummary(outcome: .completed, outputDirectory: item.outputDirectory, dataBytes: info.dataBytes - 1_000, elapsedSeconds: 60)
    item.state = .finished(summary)
    #expect(line(item).hasPrefix("\(size) · "))
    #expect(DetailRow.rows(for: item, locale: locale).first { $0.label == "Size" }?.value == size)
  }

  @Test("Headlines and tones")
  func headlinesAndTones() {
    #expect(StatusText.headline(for: PreviewData.needsAttentionUnrepairable) == "Needs Attention")
    #expect(StatusText.headline(for: PreviewData.needsPassword) == "Password Required")
    #expect(StatusText.headline(for: PreviewData.needsSpace) == "Not Enough Space")
    #expect(StatusText.headline(for: PreviewData.downloading) == nil)
    #expect(StatusText.tone(for: PreviewData.downloading) == .active)
    #expect(StatusText.tone(for: PreviewData.finished) == .good)
    #expect(StatusText.tone(for: PreviewData.finishedWithIssues) == .warning)
    #expect(StatusText.tone(for: PreviewData.failed) == .bad)
    #expect(StatusText.tone(for: PreviewData.needsPassword) == .warning)
    #expect(StatusText.tone(for: PreviewData.queued) == .neutral)
  }

  @Test("The window subtitle sums up the queue, and names post-processing for what it is")
  func queueSummary() {
    func summary(_ downloading: Int, _ queued: Int, _ paused: Int, _ isPaused: Bool, _ speed: Double, processing: [JobPhase] = []) -> String {
      StatusText.queueSummary(
        downloading: downloading, processing: processing, queued: queued, paused: paused, isPaused: isPaused, speed: speed, locale: locale)
    }
    #expect(summary(2, 1, 0, false, 84_000_000) == "2 downloading · 84 MB/s")
    #expect(summary(1, 0, 0, false, 0) == "1 downloading")
    #expect(summary(0, 3, 0, true, 0) == "Paused · 3 waiting")
    #expect(summary(0, 0, 1, false, 0) == "Paused")
    #expect(summary(0, 3, 0, false, 0) == "3 waiting")
    #expect(summary(0, 0, 0, false, 0) == "")
    // The only job is extracting: not "1 downloading".
    #expect(summary(0, 0, 0, false, 0, processing: [.extracting]) == "1 extracting")
    #expect(summary(0, 2, 0, false, 0, processing: [.verifying]) == "1 verifying · 2 waiting")
    #expect(summary(1, 2, 0, false, 84_000_000, processing: [.repairing]) == "1 downloading · 84 MB/s · 1 repairing")
    #expect(summary(0, 0, 0, false, 0, processing: [.verifying, .extracting]) == "2 processing")
    #expect(summary(0, 1, 0, true, 0, processing: [.extracting]) == "Paused · 1 extracting · 1 waiting")
  }

  @Test("The queue's own summary counts downloads apart from post-processing")
  @MainActor
  func queueSummaryForQueue() {
    let queue = DownloadQueue.preview(items: [PreviewData.extracting, PreviewData.queued])
    #expect(StatusText.queueSummary(for: queue, locale: locale) == "1 extracting · 1 waiting")
    let busy = DownloadQueue.preview(items: [PreviewData.downloading, PreviewData.extracting, PreviewData.queued])
    #expect(StatusText.queueSummary(for: busy, locale: locale) == "1 downloading · 84 MB/s · 1 extracting")
    // The menu bar names the downloading item, then the rest.
    #expect(StatusText.queueSummary(for: busy, excluding: PreviewData.downloading.id, locale: locale) == "1 extracting · 1 waiting")
  }

  // MARK: Formatters

  @Test("Sizes are Finder's")
  func bytes() {
    #expect(Format.bytes(8_200_267_250, locale: locale) == "8.2 GB")
    #expect(Format.bytes(140_000_000, locale: locale) == "140 MB")
    #expect(Format.bytes(512, locale: locale) == "512 bytes")
    #expect(Format.bytes(1, locale: locale) == "1 byte")
    // One rule for precision: a decimal below 10, none from 10 up.
    #expect(Format.bytes(459_400_000, locale: locale) == "459 MB")
    #expect(Format.bytes(960_000_000, locale: locale) == "960 MB")
    #expect(Format.bytes(6_090_000_000, locale: locale) == "6.1 GB")
    #expect(Format.bytes(2_000_000_000, locale: locale) == "2 GB")
    #expect(Format.bytes(999_600_000, locale: locale) == "1 GB")
    #expect(Format.bytes(9_960_000, locale: locale) == "10 MB")
    #expect(Format.bytes(1_500, locale: locale) == "1.5 kB")
    // A count that climbs keeps its decimal.
    #expect(Format.bytes(2_000_000_000, of: 3_300_000_000, locale: locale) == "2.0 GB of 3.3 GB")
    #expect(Format.bytes(412_000_000, of: 1_032_000_000, locale: locale) == "412 MB of 1.0 GB")
    #expect(Format.bytes(0, locale: locale) == "0 bytes")
    #expect(Format.bytes(-5, locale: locale) == "0 bytes")
    #expect(Format.bytes(3_100_000_000, of: 8_200_000_000, locale: locale) == "3.1 GB of 8.2 GB")
    // German uses a decimal comma (and a no-break space before the unit).
    #expect(Format.bytes(3_100_000_000, locale: Locale(identifier: "de_DE")).hasPrefix("3,1"))
  }

  @Test("Speeds round so they do not flicker")
  func speeds() {
    #expect(Format.speed(84_412_345, locale: locale) == "84 MB/s")
    #expect(Format.speed(84_612_345, locale: locale) == "85 MB/s")
    #expect(Format.speed(8_249_000, locale: locale) == "8.2 MB/s")
    #expect(Format.speed(512_400, locale: locale) == "512 kB/s")
    #expect(Format.speed(1_234_000_000, locale: locale) == "1.2 GB/s")
    #expect(Format.speed(0, locale: locale) == "0 bytes/s")
    #expect(Format.speed(.nan, locale: locale) == Format.speed(0, locale: locale))
    #expect(Format.speed(-3, locale: locale) == Format.speed(0, locale: locale))
  }

  @Test("Durations are Apple's abbreviations, one unit under an hour")
  func durations() {
    #expect(Format.duration(45, locale: locale) == "45 sec")
    #expect(Format.duration(0.2, locale: locale) == "1 sec")
    #expect(Format.duration(75, locale: locale) == "1 min")
    #expect(Format.duration(184, locale: locale) == "3 min")
    #expect(Format.duration(3_725, locale: locale) == "1 hr, 2 min")
    #expect(Format.duration(2 * 86_400 + 3 * 3_600, locale: locale) == "2 days, 3 hr")
    #expect(Format.duration(.infinity, locale: locale) == "1 sec")
    #expect(Format.timeLeft(60, locale: locale) == "1 min left")
    #expect(Format.timeLeft(30, locale: locale) == "30 sec left")
  }

  @Test("Percentages, shares and counts")
  func numbers() {
    #expect(Format.percent(0.43, locale: locale) == "43%")
    #expect(Format.percent(1.7, locale: locale) == "100%")
    #expect(Format.share(0.09, locale: locale) == "9%")
    #expect(Format.share(0.0004, locale: locale) == "Less than 1%")
    #expect(Format.share(0, locale: locale) == "0%")
    #expect(Format.count(21_840, locale: locale) == "21,840")
    #expect(Format.count(1, "block", "blocks", locale: locale) == "1 block")
    #expect(Format.count(12, "block", "blocks", locale: locale) == "12 blocks")
  }
}
