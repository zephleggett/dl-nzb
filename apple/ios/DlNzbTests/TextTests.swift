import DlNzbKit
import Foundation
import Testing

@testable import DlNzbApp

@Suite("Row and Live Activity text")
struct TextTests {
  let locale = Locale(identifier: "en_US")

  @Test("Sizes share their unit when they can")
  func compactBytes() {
    #expect(ContinuedTaskText.compactBytes(3_100_000_000, of: 8_200_000_000, locale: locale) == "3.1 of 8.2 GB")
    #expect(ContinuedTaskText.compactBytes(512_000_000, of: 8_200_000_000, locale: locale) == "512 MB of 8.2 GB")
  }

  @Test("A downloading row leaves the speed to the subtitle")
  func downloadingRow() {
    let item = Items.downloading(1, done: 3_100_000_000, of: 8_200_000_000)
    #expect(RowStatus.line(for: item, locale: locale) == "3.1 of 8.2 GB · 30 sec left")
  }

  @Test("A finished row says what was repaired, or how long it took")
  func finishedRow() {
    var summary = JobSummary(outcome: .completed, outputDirectory: URL(filePath: "/tmp"), elapsedSeconds: 184)
    #expect(RowStatus.line(for: Items.make(1, .finished(summary), bytes: 8_200_000_000), locale: locale) == "8.2 GB · Finished in 3 min")
    summary.par2 = Par2Report(ran: true, damagedBlocks: 12, repairedBlocks: 12, repaired: true)
    #expect(RowStatus.line(for: Items.make(1, .finished(summary), bytes: 8_200_000_000), locale: locale) == "8.2 GB · Repaired 12 blocks")
  }

  @Test("The Live Activity names the release and counts what waits behind it")
  func liveActivityText() {
    let downloading = Items.downloading(1, done: 3_100_000_000, of: 8_200_000_000, title: "Sintel.2010.2160p.UHD.BluRay.x265")
    #expect(ContinuedTaskText.title(for: downloading) == "Sintel 2010 2160p UHD BluRay x265")
    #expect(ContinuedTaskText.subtitle(for: downloading, locale: locale) == "3.1 of 8.2 GB")
    #expect(ContinuedTaskText.subtitle(for: downloading, waiting: 2, locale: locale) == "3.1 of 8.2 GB · 2 waiting")

    let extracting = Items.make(
      2, .running(.extracting), progress: JobProgress(phase: .extracting, filesDone: 1, filesTotal: 5, fraction: 0.3), title: "Tears_of_Steel")
    #expect(ContinuedTaskText.title(for: extracting) == "Tears of Steel")
    #expect(ContinuedTaskText.subtitle(for: extracting, waiting: 1, locale: locale) == "Extracting · 2 of 5 · 1 waiting")

    #expect(ContinuedTaskText.title(for: nil) == "Downloading")
    #expect(ContinuedTaskText.subtitle(for: nil, waiting: 3, locale: locale) == "Starting")
  }

  @Test("A stopped download reads Stopped, never Cancelled")
  func stoppedRow() {
    let progress = JobProgress(phase: .downloading, bytesDone: 1_200_000_000, bytesTotal: 8_200_000_000)
    let stopped = Items.make(1, .stopped, bytes: 8_200_000_000, progress: progress)
    #expect(RowStatus.title(for: stopped) == "Stopped")
    #expect(RowStatus.line(for: stopped, locale: locale) == "Stopped · 1.2 of 8.2 GB")
    #expect(!RowStatus.line(for: Items.make(2, .stopped), locale: locale).contains("Cancelled"))
  }

  @Test("A waiting row the queue holds back reads Paused")
  func heldRow() {
    let waiting = Items.make(1, .queued, bytes: 5_800_000_000)
    #expect(RowStatus.line(for: waiting, locale: locale) == "Waiting · 5.8 GB")
    #expect(RowStatus.line(for: waiting, held: true, locale: locale) == "Paused · 5.8 GB")
    #expect(RowStatus.title(for: waiting) == "Waiting")
    #expect(RowStatus.title(for: waiting, held: true) == "Paused")
  }

  @Test("The paused notice says what happened and what to do")
  func pausedNotice() {
    let content = Notifier.pausedContent()
    #expect(content.title == "Downloads Paused")
    #expect(content.body == "Open dl-nzb to continue.")
    #expect(content.userInfo.isEmpty)
  }

  @Test("Run progress weighs items by size and never counts a finished one short")
  func runProgress() {
    let finished = Items.make(1, .finished(JobSummary(outcome: .completed, outputDirectory: URL(filePath: "/tmp"))), bytes: 6_000)
    let half = Items.downloading(2, done: 1_000, of: 2_000)
    let waiting = Items.make(3, .queued, bytes: 2_000)
    let progress = RunProgress.of([finished, half, waiting])
    #expect(progress.total == 10_000)
    #expect(progress.completed == 6_000 + Int64(2_000 * 0.5 * RunProgress.downloadShare))

    // Post-processing keeps moving the bar after the bytes are in.
    var repairing = Items.make(4, .running(.repairing), bytes: 1_000, progress: JobProgress(phase: .repairing, fraction: 0))
    let before = RunProgress.of([repairing]).completed
    repairing.progress = JobProgress(phase: .repairing, fraction: 1)
    #expect(RunProgress.of([repairing]).completed > before)
    #expect(before >= Int64(1_000 * RunProgress.downloadShare))
  }
}
