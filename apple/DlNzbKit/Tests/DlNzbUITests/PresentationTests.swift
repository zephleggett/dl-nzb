import AppKit
import Foundation
import Testing
import UniformTypeIdentifiers
import UserNotifications

@testable import DlNzbKit
@testable import DlNzbUI

/// The rest of the shared presentation: bars, glyphs, symbols, the phase
/// checklist, details, Test Connection's states and the acknowledgements.
@Suite("Presentation")
struct PresentationTests {
  let locale = Locale(identifier: "en_US")

  @Test("Bars follow the SPEC: none, indeterminate, bytes, phase fraction or frozen")
  func bars() {
    #expect(RowProgress.of(PreviewData.queued) == .none)
    #expect(RowProgress.of(PreviewData.connecting) == .indeterminate)
    #expect(RowProgress.of(PreviewData.checking) == .indeterminate)
    guard case .determinate(let downloaded) = RowProgress.of(PreviewData.downloading) else {
      Issue.record("downloading should have a bar")
      return
    }
    #expect(abs(downloaded - 3_100_000_000 / 8_180_030_936) < 0.0001)
    #expect(RowProgress.of(PreviewData.repairing) == .determinate(0.43))
    #expect(RowProgress.of(PreviewData.paused) == .frozen(1_900_000_000 / 4_645_334_117))
    for item in [PreviewData.finished, PreviewData.failed, PreviewData.needsPassword, PreviewData.stopped] {
      #expect(RowProgress.of(item) == .none)
    }
  }

  @Test("Glyphs mark the ends: green check, orange warning, red cross")
  func glyphs() {
    #expect(StatusSymbol.of(PreviewData.finished)?.name == "checkmark.circle.fill")
    #expect(StatusSymbol.of(PreviewData.finished)?.tone == .good)
    #expect(StatusSymbol.of(PreviewData.finishedWithIssues)?.tone == .warning)
    #expect(StatusSymbol.of(PreviewData.failed)?.tone == .bad)
    #expect(StatusSymbol.of(PreviewData.needsPassword)?.name == "lock.fill")
    #expect(StatusSymbol.of(PreviewData.downloading) == nil)
    #expect(StatusSymbol.of(PreviewData.queued) == nil)
  }

  @Test("Every symbol the views use exists in this system's SF Symbols")
  @MainActor
  func symbolsExist() {
    var names = ContentKind.allCases.map(\.symbolName)
    names += PreviewData.allStates.compactMap { StatusSymbol.of($0)?.name }
    names += [
      "pause.fill", "play.fill", "stop.fill", "circle", "pause.circle", "stop.circle", "checkmark.circle.fill", "minus.circle",
      "exclamationmark.circle.fill", "xmark.circle.fill",
    ]
    for name in Set(names) {
      #expect(NSImage(systemSymbolName: name, accessibilityDescription: nil) != nil, "\(name)")
    }
  }

  @Test("Content kinds have their own symbols, and archives of films look like films")
  func contentSymbols() {
    #expect(ContentKind.video.symbolName == "film")
    #expect(ContentKind.audio.symbolName == "music.note")
    #expect(Set(ContentKind.allCases.map(\.symbolName)).count == ContentKind.allCases.count)
    #expect(PreviewData.extracting.contentKind == .video)
  }

  // MARK: Phase checklist

  private func statuses(_ item: DownloadItem) -> [PhaseStep.Status] {
    PhaseStep.steps(for: item, locale: locale).map(\.status)
  }

  @Test("While repairing: checked and downloaded, verified, repairing, extract to come")
  func checklistRunning() {
    let steps = PhaseStep.steps(for: PreviewData.repairing, locale: locale)
    #expect(steps.map(\.kind) == PhaseStep.Kind.allCases)
    #expect(steps.map(\.status) == [.done, .done, .done, .active(0.43), .pending])
    #expect(steps[3].detail == "12 blocks")
    #expect(statuses(PreviewData.connecting) == [.pending, .pending, .pending, .pending, .pending])
    #expect(statuses(PreviewData.checking) == [.active(0.38), .pending, .pending, .pending, .pending])
  }

  @Test("Once finished: what ran is done, what was not needed is skipped")
  func checklistFinished() {
    let finished = PhaseStep.steps(for: PreviewData.finished, locale: locale)
    #expect(finished.map(\.status) == [.done, .done, .done, .done, .skipped])
    #expect(finished[3].detail == "12 blocks")

    var clean = PreviewData.finished
    let summary = JobSummary(outcome: .completed, outputDirectory: clean.outputDirectory, par2: Par2Report(ran: true, verifiedOK: true), archivesExtracted: 1)
    clean.state = .finished(summary)
    clean.visitedPhases = [.connecting, .downloading, .verifying, .extracting]
    let steps = PhaseStep.steps(for: clean, locale: locale)
    #expect(steps.map(\.status) == [.skipped, .done, .done, .skipped, .done])
    #expect(steps[3].detail == "Not needed")

    #expect(statuses(PreviewData.finishedWithIssues) == [.skipped, .done, .done, .failed, .failed])
  }

  @Test("Stuck and stopped jobs show where they stopped")
  func checklistStuck() {
    #expect(statuses(PreviewData.needsAttentionUnrepairable) == [.attention, .pending, .pending, .pending, .pending])
    #expect(statuses(PreviewData.needsPassword) == [.done, .done, .skipped, .skipped, .attention])
    #expect(statuses(PreviewData.failed) == [.done, .done, .done, .failed, .pending])
    #expect(statuses(PreviewData.paused) == [.done, .interrupted(1_900_000_000 / 4_645_334_117), .pending, .pending, .pending])
    #expect(statuses(PreviewData.queued) == [.pending, .pending, .pending, .pending, .pending])
    // Stopped is its own state, not a pause.
    #expect(statuses(PreviewData.stopped) == [.done, .stopped(412_000_000 / 1_032_366_025), .pending, .pending, .pending])
  }

  // MARK: Details

  @Test("Details list the facts worth showing")
  func details() {
    let rows = DetailRow.rows(for: PreviewData.finished, locale: locale)
    let labels = rows.map(\.label)
    #expect(labels.starts(with: ["Size", "Average Speed", "Time", "Articles Missing", "Blocks Repaired", "Category"]))
    #expect(rows.first { $0.label == "Articles Missing" }?.value == "46 of 11,090")
    #expect(rows.first { $0.label == "Time" }?.value == "3 min")
    #expect(labels.contains("Finished"))
    let waiting = DetailRow.rows(for: PreviewData.queued, locale: locale).map(\.label)
    #expect(!waiting.contains("Time") && !waiting.contains("Finished"))
  }

  // MARK: Test Connection

  @Test("Test Connection's result reads as the SPEC asks")
  @MainActor
  func connectionTest() async {
    let success = await ConnectionTestState.run { ServerCheck(greeting: "200 hi", tls: true, latencyMilliseconds: 38) }
    #expect(success == .success(latencyMilliseconds: 38, tls: true))
    #expect(success.text == "Connected · 38 ms")
    let plain = ConnectionTestState.success(latencyMilliseconds: 12, tls: false)
    #expect(plain.text == "Connected without encryption · 12 ms")
    let failure = await ConnectionTestState.run { throw EngineError(.auth) }
    #expect(failure == .failure(EngineError.Kind.auth.defaultMessage))
    let cancelled = await ConnectionTestState.run { throw CancellationError() }
    #expect(cancelled == .idle)
  }

  // MARK: Acknowledgements

  @Test(
    "The unRAR paragraph is verbatim from its licence",
    .enabled(if: FileManager.default.fileExists(atPath: PresentationTests.unrarLicence.path(percentEncoded: false))))
  func unrarParagraph() throws {
    let licence = try String(contentsOf: Self.unrarLicence, encoding: .utf8)
    let start = try #require(licence.range(of: "UnRAR source code may be used"))
    let end = try #require(licence.range(of: "resulting package.", range: start.lowerBound..<licence.endIndex))
    let paragraph = licence[start.lowerBound..<end.upperBound].split(whereSeparator: \.isWhitespace).joined(separator: " ")
    #expect(Acknowledgement.unrarNotice == paragraph)
  }

  static let unrarLicence = FileManager.default.homeDirectoryForCurrentUser.appending(
    path: ".cargo/registry/src/index.crates.io-6f17d22bba15001f/unrar_sys-0.5.8/vendor/unrar/license.txt")

  @Test("A wrapped release name breaks between its parts, and reads the same")
  func breakableNames() {
    let name = "Sintel.2010_2160p"
    let breakable = ReleaseText.breakable(name)
    #expect(breakable.replacingOccurrences(of: "\u{200B}", with: "") == name)
    #expect(breakable.components(separatedBy: "\u{200B}").count == 3)
  }

  @Test("VoiceOver hears a release name as words, keeping numbers whole")
  func spokenNames() {
    #expect(ReleaseText.spoken("Tears.of.Steel.2012.1080p.WEB-DL") == "Tears of Steel 2012 1080p WEB-DL")
    #expect(ReleaseText.spoken("The_Linux_Command_Line_2nd_Edition") == "The Linux Command Line 2nd Edition")
    #expect(ReleaseText.spoken("debian-12.7.0-amd64-DVD-1") == "debian-12.7.0-amd64-DVD-1")
    #expect(ReleaseText.spoken("Movie.DTS-HD.MA.5.1.x265") == "Movie DTS-HD MA 5.1 x265")
    #expect(ReleaseText.spoken(".hidden..name.") == "hidden name")
    #expect(!ReleaseText.spoken("Sintel.2010").contains("\u{200B}"))
  }

  @Test("Notifications say what happened, with the size once finished or the problem otherwise")
  func notifications() throws {
    let finished = try #require(NotificationText(PreviewData.finished, locale: locale))
    #expect(finished.title == "Download Finished")
    // Plain text: no invisible break characters in a notification.
    #expect(finished.body == "\(PreviewData.sintelTitle) · 8.2 GB")
    #expect(NotificationText(PreviewData.finishedWithIssues)?.title == "Download Finished with Problems")
    let failed = try #require(NotificationText(PreviewData.failed, locale: locale))
    #expect(failed.title == "Download Failed")
    #expect(failed.body.hasSuffix(" · 9% of articles missing"))
    #expect(NotificationText(PreviewData.needsPassword)?.title == "Password Required")
    #expect(NotificationText(PreviewData.needsSpace)?.title == "Not Enough Space")
    for item in [PreviewData.queued, PreviewData.downloading, PreviewData.paused, PreviewData.stopped] {
      #expect(NotificationText(item) == nil)
    }
  }

  @Test("A notification carries its item, which a click finds again from either key")
  func notificationContent() throws {
    let content = try #require(NotificationText.content(for: PreviewData.finished, locale: locale))
    #expect(content.title == "Download Finished")
    #expect(content.body == "\(PreviewData.sintelTitle) · 8.2 GB")
    #expect(content.threadIdentifier == "downloads")
    #expect(NotificationText.itemID(from: content.userInfo) == PreviewData.finished.id)
    // What the Mac app wrote before both apps shared the key.
    #expect(NotificationText.itemID(from: ["item": PreviewData.failed.id.uuidString]) == PreviewData.failed.id)
    #expect(NotificationText.itemID(from: [:]) == nil)
    #expect(NotificationText.content(for: PreviewData.downloading) == nil)
  }

  @Test("Queue alerts name the server problem and tell a waiting duplicate from a downloaded one")
  @MainActor
  func alerts() {
    #expect(AlertText.serverProblemTitle(.auth) == "Couldn’t Log In to the Server")
    #expect(AlertText.serverProblemTitle(nil) == "Couldn’t Reach the Server")
    #expect(AlertText.serverProblemMessage(EngineError(.auth)).hasSuffix(" Downloads are paused until the server works again."))
    let queue = DownloadQueue.preview(items: [PreviewData.queued, PreviewData.finished])
    let folder = URL(filePath: "/Users/me/Downloads/Sintel", directoryHint: .isDirectory)
    let waiting = DuplicateNZB(title: "Sintel", existingItemID: PreviewData.queued.id, folder: folder, data: Data(), fingerprint: "", fileName: "Sintel.nzb")
    #expect(AlertText.duplicateTitle(waiting, in: queue) == "Already in the List")
    #expect(AlertText.duplicateMessage(waiting, in: queue) == "“Sintel” is in the list already.")
    #expect(AlertText.duplicateShowTitle(waiting, in: queue) == "Show in List")
    #expect(queue.listedItem(for: waiting)?.id == PreviewData.queued.id)
    let done = DuplicateNZB(title: "Sintel", existingItemID: PreviewData.finished.id, folder: folder, data: Data(), fingerprint: "", fileName: "Sintel.nzb")
    #expect(AlertText.duplicateTitle(done, in: queue) == "Already Downloaded")
    #expect(AlertText.duplicateMessage(done, in: queue) == "“Sintel” has been downloaded before. Its files are in the Downloads folder.")
    #expect(AlertText.duplicateShowTitle(done, in: queue) == "Show in Finder")
    #expect(queue.listedItem(for: done) == nil)
    let long = DuplicateNZB(title: "Tears.of.Steel", existingItemID: nil, folder: folder, data: Data(), fingerprint: "", fileName: "x.nzb")
    #expect(!AlertText.duplicateMessage(long, in: queue).contains("\u{200B}"))
  }

  @Test("Files that could not be added are named, or counted")
  func openFailures() {
    let one = OpenFailure(fileName: "notes.nzb", message: "This is not an NZB.")
    #expect(AlertText.openFailureTitle([one], locale: locale) == "Couldn’t Open “notes.nzb”")
    #expect(AlertText.openFailureTitle(Array(repeating: one, count: 1_200), locale: locale) == "Couldn’t Open 1,200 Files")
  }

  // MARK: Files

  @Test("NZBs are told by their extension and declared as XML")
  func nzbType() {
    #expect(URL(filePath: "/tmp/Sintel.NZB").isNZB)
    #expect(!URL(filePath: "/tmp/Sintel.xml").isNZB)
    #expect(UTType.nzb.identifier == "com.zephleggett.dl-nzb.nzb")
    #expect(UTType.nzb.conforms(to: .xml))
  }

  @Test("The main file is the largest one, when it holds enough of the download and is there")
  func mainFile() throws {
    let folder = FileManager.default.temporaryDirectory.appending(path: "dl-nzb-main-file-\(UUID().uuidString)", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: folder) }
    var item = PreviewData.finished
    item.summary = JobSummary(
      outcome: .completed, outputDirectory: folder, files: [OutputFile(name: "Film.nfo", bytes: 15), OutputFile(name: "Film.mkv", bytes: 85)])
    let film = folder.appending(path: "Film.mkv", directoryHint: .notDirectory)
    #expect(item.largestFile == film)
    #expect(item.mainFile(whenShare: { $0 > 0.8 }) == nil)
    try Data().write(to: film)
    #expect(item.mainFile(whenShare: { $0 > 0.8 }) == film)
    #expect(item.mainFile(whenShare: { $0 >= 0.9 }) == nil)
    item.summary?.files = []
    #expect(item.largestFile == nil)
    #expect(item.mainFile(whenShare: { _ in true }) == nil)
  }

  @Test("Progress alone leaves an item equal for the views that do not show it")
  func equalsIgnoringProgress() {
    var moved = PreviewData.downloading
    moved.progress?.bytesDone += 1_000_000
    #expect(moved != PreviewData.downloading)
    #expect(moved.equalsIgnoringProgress(PreviewData.downloading))
    var paused = moved
    paused.state = .paused
    #expect(!paused.equalsIgnoringProgress(PreviewData.downloading))
  }

  // MARK: Tone

  /// WCAG 2 contrast between two sRGB colours, components 0...1.
  private func contrast(_ a: (Double, Double, Double), _ b: (Double, Double, Double)) -> Double {
    func linear(_ c: Double) -> Double { c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4) }
    func luminance(_ c: (Double, Double, Double)) -> Double { 0.2126 * linear(c.0) + 0.7152 * linear(c.1) + 0.0722 * linear(c.2) }
    let (high, low) = (max(luminance(a), luminance(b)), min(luminance(a), luminance(b)))
    return (high + 0.05) / (low + 0.05)
  }

  @Test("Tinted words keep 4.5:1 in light mode on white, the window grey and the grouped-list grey")
  func toneContrast() throws {
    let backgrounds: [(Double, Double, Double)] = [(1, 1, 1), (236 / 255, 236 / 255, 236 / 255), (242 / 255, 242 / 255, 247 / 255)]
    for tone in [StatusTone.good, .warning, .bad] {
      let rgb = try #require(tone.lightTextRGB)
      for background in backgrounds {
        #expect(contrast((rgb.red, rgb.green, rgb.blue), background) >= 4.5, "\(tone)")
      }
      // Light mode at standard contrast takes the readable shade; Dark Mode
      // and Increase Contrast keep the system colour, which passes there.
      #expect(tone.textColor(colorScheme: .light, contrast: .standard) != tone.color)
      #expect(tone.textColor(colorScheme: .dark, contrast: .standard) == tone.color)
      #expect(tone.textColor(colorScheme: .light, contrast: .increased) == tone.color)
    }
    // The system colours themselves fail as text on white: why the shades exist.
    #expect(contrast((1, 149 / 255, 0), (1, 1, 1)) < 4.5)
    #expect(StatusTone.neutral.lightTextRGB == nil && StatusTone.active.lightTextRGB == nil)
    #expect(StatusTone.neutral.textColor(colorScheme: .light, contrast: .standard) == nil)
  }

  @Test("Status lines tint only problems; emphasis tints good news too; glyphs take the system colour")
  func toneRoles() {
    #expect(StatusTone.good.textStyle.textTone == nil)
    #expect(StatusTone.active.textStyle.textTone == nil)
    #expect(StatusTone.warning.textStyle.textTone == .warning)
    #expect(StatusTone.bad.textStyle.textTone == .bad)
    #expect(StatusTone.good.emphasisStyle.textTone == .good)
    #expect(StatusTone.neutral.emphasisStyle.textTone == nil)
    #expect(StatusTone.warning.glyphStyle.color == .orange)
    #expect(StatusTone.neutral.glyphStyle.color == nil)
  }

  @Test("The bundled crate list loads, empty until the engine build fills it")
  func bundledCrates() {
    #expect(Acknowledgement.bundledRustCrates.allSatisfy { !$0.name.isEmpty && !$0.licence.isEmpty })
  }
}
