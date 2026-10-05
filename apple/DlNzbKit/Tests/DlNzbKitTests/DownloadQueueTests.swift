import Foundation
import Observation
import Synchronization
import Testing

@testable import DlNzbKit

/// The queue, end to end on the simulated engine: adding, turn-taking, the
/// controls, the follow-ups, server problems, persistence and retention.
@MainActor
@Suite("Download queue")
struct DownloadQueueTests {
  // MARK: Adding and finishing

  @Test("An added NZB is copied into the queue, runs and finishes in its own folder")
  func addRunFinish() async throws {
    let harness = QueueHarness()
    try await harness.launch()
    var notified: [UUID] = []
    harness.queue.onItemFinished = { notified.append($0.id) }
    let id = try await harness.add("Clean.Release.2024.1080p.WEB-DL")
    let item = try #require(harness.queue.item(id))
    #expect(item.title == "Clean.Release.2024.1080p.WEB-DL")
    #expect(item.nzbURL == harness.storage.nzbURL(for: id))
    #expect(FileManager.default.fileExists(atPath: item.nzbURL.path(percentEncoded: false)))
    #expect(item.outputDirectory.lastPathComponent == "Clean.Release.2024.1080p.WEB-DL")
    #expect(item.outputDirectory.deletingLastPathComponent().standardizedFileURL == harness.downloads.standardizedFileURL)
    #expect(item.info?.totalBytes == 108_000_000)

    #expect(await eventually { harness.isFinished(id) })
    let finished = try #require(harness.queue.item(id))
    #expect(finished.finishedAt != nil)
    #expect(finished.visitedPhases.first == .connecting)
    #expect(finished.visitedPhases.contains(.downloading))
    #expect(notified == [id])
    #expect(FileManager.default.fileExists(atPath: finished.outputDirectory.path(percentEncoded: false)))
    #expect(harness.queue.activeCount == 0 && harness.queue.overallFraction == nil)
  }

  @Test("A file that is not an NZB is turned away with a sentence")
  func invalidFile() async throws {
    let harness = QueueHarness()
    try await harness.launch()
    let url = harness.inbox.appending(path: "notes.nzb")
    try Data("not xml".utf8).write(to: url)
    let result = await harness.queue.add(url)
    guard case .failed(let name, let message) = result else {
      Issue.record("expected a failure, got \(result)")
      return
    }
    #expect(name == "notes.nzb")
    #expect(message.hasSuffix("."))
    #expect(harness.queue.items.isEmpty)
    #expect(((try? FileManager.default.contentsOfDirectory(atPath: harness.storage.nzbDirectory.path(percentEncoded: false))) ?? []).isEmpty)
  }

  // MARK: Duplicates and folders

  @Test("Opening the same NZB again asks, and Download Again uses a fresh folder")
  func duplicates() async throws {
    let harness = QueueHarness(startAutomatically: false)
    try await harness.launch()
    let url = try TestNZB.write(title: "Twice.Opened.Release", in: harness.inbox)
    guard case .added(let first) = await harness.queue.add(url) else { throw CancellationError() }
    let again = await harness.queue.add(url)
    guard case .duplicate(let duplicate) = again else {
      Issue.record("expected a duplicate, got \(again)")
      return
    }
    #expect(duplicate.existingItemID == first)
    #expect(duplicate.folder == harness.queue.item(first)?.outputDirectory)
    #expect(harness.queue.items.count == 1)

    guard case .added(let second) = await harness.queue.addAgain(duplicate) else { throw CancellationError() }
    #expect(harness.queue.item(second)?.outputDirectory.lastPathComponent == "Twice.Opened.Release 2")
    // The copy's row can be told apart; the release name stays the engine's.
    #expect(harness.queue.item(first)?.displayTitle == "Twice.Opened.Release")
    #expect(harness.queue.item(second)?.displayTitle == "Twice.Opened.Release (2)")
    #expect(harness.queue.item(second)?.title == "Twice.Opened.Release")
  }

  @Test("Opening an NZB again points at its unfinished copy, not a finished one, and offers it in the list")
  func duplicatePrefersUnfinished() async throws {
    let harness = QueueHarness(startAutomatically: false)
    try await harness.launch()
    let url = try TestNZB.write(title: "Copies.Of.Release", in: harness.inbox)
    guard case .added(let first) = await harness.queue.add(url) else { throw CancellationError() }
    harness.queue.start(first)
    #expect(await eventually { harness.isFinished(first) })
    guard case .duplicate(let toFinished) = await harness.queue.add(url) else { throw CancellationError() }
    #expect(toFinished.existingItemID == first)
    #expect(harness.queue.listedItem(for: toFinished) == nil)

    guard case .added(let second) = await harness.queue.addAgain(toFinished) else { throw CancellationError() }
    guard case .duplicate(let toWaiting) = await harness.queue.add(url) else { throw CancellationError() }
    #expect(toWaiting.existingItemID == second)
    #expect(harness.queue.listedItem(for: toWaiting)?.id == second)
  }

  @Test("Two NZBs of the same release get Name and Name 2")
  func folderNames() async throws {
    let harness = QueueHarness(startAutomatically: false)
    try await harness.launch()
    let one = try TestNZB.write(title: "Same.Name", in: harness.scratch.folder("A"), salt: "a")
    let two = try TestNZB.write(title: "Same.Name", in: harness.scratch.folder("B"), salt: "b")
    guard case .added(let first) = await harness.queue.add(one), case .added(let second) = await harness.queue.add(two) else {
      Issue.record("both should be added")
      return
    }
    #expect(harness.queue.item(first)?.outputDirectory.lastPathComponent == "Same.Name")
    #expect(harness.queue.item(second)?.outputDirectory.lastPathComponent == "Same.Name 2")
  }

  @Test("A release whose folder already holds files counts as downloaded already")
  func alreadyDownloaded() async throws {
    let harness = QueueHarness(startAutomatically: false)
    try await harness.launch()
    let existing = harness.downloads.appending(path: "Old.Release", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: existing, withIntermediateDirectories: true)
    try Data("x".utf8).write(to: existing.appending(path: "movie.mkv"))
    let result = await harness.queue.add(try TestNZB.write(title: "Old.Release", in: harness.inbox))
    guard case .duplicate(let duplicate) = result else {
      Issue.record("expected a duplicate, got \(result)")
      return
    }
    #expect(duplicate.existingItemID == nil)
    #expect(duplicate.folder.standardizedFileURL == existing.standardizedFileURL)
    guard case .added(let id) = await harness.queue.addAgain(duplicate) else { throw CancellationError() }
    #expect(harness.queue.item(id)?.outputDirectory.lastPathComponent == "Old.Release 2")
  }

  // MARK: Turn-taking

  @Test("Never two jobs in network phases at once, and every job finishes")
  func oneAtATime() async throws {
    let harness = QueueHarness(timeScale: 1_000)
    harness.engine.scenarioOverride = .repair
    try await harness.launch()
    var ids: [UUID] = []
    for number in 1...4 {
      ids.append(try await harness.add("Turn.Taking.Release.\(number)", dataBytes: 60_000_000))
    }
    var peakInQueue = 0
    let done = await eventually(timeout: .seconds(30)) {
      peakInQueue = max(peakInQueue, harness.queue.items.count(where: \.usesNetwork))
      return ids.allSatisfy { harness.isFinished($0) }
    }
    #expect(done)
    #expect(peakInQueue <= 1)
    #expect(harness.engine.peakNetworkJobCount == 1)
    #expect(harness.engine.startedJobCount == 4)
  }

  @Test("With automatic start off, items wait for Start, in the list's order")
  func manualStartAndOrder() async throws {
    let harness = QueueHarness(startAutomatically: false)
    try await harness.launch()
    let a = try await harness.add("Order.A")
    let b = try await harness.add("Order.B")
    let c = try await harness.add("Order.C")
    try await Task.sleep(for: .milliseconds(50))
    #expect(harness.queue.items.allSatisfy { $0.isQueued })
    #expect(harness.queue.awaitsStart(try #require(harness.queue.item(a))))
    #expect(harness.engine.startedJobCount == 0)

    harness.queue.move(fromOffsets: [2], toOffset: 0)
    #expect(harness.queue.items.map(\.id) == [c, a, b])
    harness.queue.startAll()
    #expect(harness.queue.items.first(where: \.isRunning)?.id == c)
    #expect(await eventually { [a, b, c].allSatisfy { harness.isFinished($0) } })
  }

  @Test("Start runs one waiting item when automatic start is off")
  func startOne() async throws {
    let harness = QueueHarness(startAutomatically: false)
    try await harness.launch()
    let a = try await harness.add("Start.A")
    let b = try await harness.add("Start.B")
    harness.queue.start(b)
    #expect(await eventually { harness.isFinished(b) })
    #expect(harness.state(a) == .queued)
    // Turning automatic start on lets the rest go.
    harness.settings.startAutomatically = true
    #expect(await eventually { harness.isFinished(a) })
  }

  // MARK: Pause, resume, stop

  @Test("Pause releases a download, the next item takes its turn, and Resume finishes it")
  func pauseResume() async throws {
    let harness = QueueHarness(timeScale: 250)
    harness.engine.scenarioOverride = .normal
    try await harness.launch()
    await harness.throttle()
    let first = try await harness.add("Pause.First")
    #expect(await harness.waitUntilDownloading(first))
    harness.queue.pause(first)
    #expect(harness.state(first) == .paused)
    #expect(await eventually { harness.engine.transferringJobCount == 0 })
    let second = try await harness.add("Pause.Second")
    #expect(await eventually { harness.queue.item(second)?.isRunning == true })

    await harness.unthrottle()
    #expect(await eventually { harness.isFinished(second) })
    #expect(harness.state(first) == .paused)
    harness.queue.resume(first)
    #expect(await eventually { harness.isFinished(first) })
    // One engine job for the first item: it was paused, not restarted.
    #expect(harness.engine.startedJobCount == 2)
  }

  @Test("Pause All holds everything and Resume All lets it go")
  func pauseAll() async throws {
    let harness = QueueHarness(timeScale: 250)
    harness.engine.scenarioOverride = .normal
    try await harness.launch()
    await harness.throttle()
    let first = try await harness.add("All.First")
    let second = try await harness.add("All.Second")
    #expect(await harness.waitUntilDownloading(first))
    harness.queue.pauseAll()
    #expect(harness.queue.isPaused && harness.queue.prefersResumeAll)
    #expect(harness.state(first) == .paused)
    try await Task.sleep(for: .milliseconds(50))
    #expect(harness.state(second) == .queued)
    #expect(harness.queue.activeCount == 0)

    await harness.unthrottle()
    harness.queue.resumeAll()
    #expect(!harness.queue.isPaused)
    #expect(await eventually { harness.isFinished(first) && harness.isFinished(second) })
  }

  @Test("Resume All can leave the items the user paused one by one where they are")
  func resumeAllKeepingPaused() async throws {
    let harness = QueueHarness(timeScale: 250)
    harness.engine.scenarioOverride = .normal
    try await harness.launch()
    await harness.throttle()
    let first = try await harness.add("Keep.First")
    let second = try await harness.add("Keep.Second")
    harness.queue.pause(second)
    #expect(await harness.waitUntilDownloading(first))
    harness.queue.pauseAll()
    #expect(harness.state(first) == .paused)

    await harness.unthrottle()
    harness.queue.resumeAll(keepingPaused: [second])
    #expect(!harness.queue.isPaused)
    #expect(await eventually { harness.isFinished(first) })
    #expect(harness.state(second) == .paused)
  }

  @Test("Cancel keeps the data so Retry continues; cancel and delete removes the folder")
  func stopKeepsOrDeletes() async throws {
    let harness = QueueHarness(timeScale: 250)
    harness.engine.scenarioOverride = .normal
    try await harness.launch()
    await harness.throttle()

    let kept = try await harness.add("Stop.Keep")
    #expect(await harness.waitUntilDownloading(kept))
    harness.queue.stop(kept)
    #expect(harness.state(kept) == .stopped)
    let keptFolder = try #require(harness.queue.item(kept)?.outputDirectory)
    #expect(await eventually { harness.engine.runningJobCount == 0 })
    #expect(FileManager.default.fileExists(atPath: keptFolder.appending(path: SimulatedSidecar.fileName).path(percentEncoded: false)))
    #expect(harness.queue.item(kept)?.canRetry == true)

    let deleted = try await harness.add("Stop.Delete")
    #expect(await harness.waitUntilDownloading(deleted))
    let deletedFolder = try #require(harness.queue.item(deleted)?.outputDirectory)
    #expect(FileManager.default.fileExists(atPath: deletedFolder.path(percentEncoded: false)))
    harness.queue.stop(deleted, deletingData: true)
    #expect(await eventually { !FileManager.default.fileExists(atPath: deletedFolder.path(percentEncoded: false)) })
    #expect(harness.state(deleted) == .stopped)
    #expect(harness.queue.item(deleted)?.progress == nil)

    await harness.unthrottle()
    harness.queue.retry(kept)
    #expect(await eventually { harness.isFinished(kept) })
  }

  @Test("Remove and Delete Data deletes what an unfinished job downloaded, never a finished one's files")
  func removeDeletingData() async throws {
    let harness = QueueHarness(timeScale: 250)
    harness.engine.scenarioOverride = .normal
    try await harness.launch()
    let finished = try await harness.add("Remove.Finished")
    #expect(await eventually { harness.isFinished(finished) })
    await harness.throttle()
    let running = try await harness.add("Remove.Running")
    #expect(await harness.waitUntilDownloading(running))
    let runningItem = try #require(harness.queue.item(running))
    #expect(runningItem.needsRemovalConfirmation)
    #expect(try !#require(harness.queue.item(finished)).needsRemovalConfirmation)
    let finishedFolder = try #require(harness.queue.item(finished)?.outputDirectory)

    harness.queue.remove(running, deletingData: true)
    harness.queue.remove(finished, deletingData: true)
    #expect(harness.queue.items.isEmpty)
    #expect(await eventually { !FileManager.default.fileExists(atPath: runningItem.outputDirectory.path(percentEncoded: false)) })
    #expect(FileManager.default.fileExists(atPath: finishedFolder.path(percentEncoded: false)))
  }

  @Test("Pause All holds waiting items, so their rows read Paused, except one the user started")
  func heldItems() async throws {
    let harness = QueueHarness(startAutomatically: false)
    try await harness.launch()
    let waiting = try await harness.add("Held.Waiting")
    let started = try await harness.add("Held.Started")
    #expect(try !harness.queue.isHeld(#require(harness.queue.item(waiting))))
    harness.queue.pauseAll()
    harness.queue.start(started)
    #expect(try harness.queue.isHeld(#require(harness.queue.item(waiting))))
    #expect(try !harness.queue.isHeld(#require(harness.queue.item(started))))
  }

  @Test("Remove keeps the files; Move to Trash takes the folder too")
  func removeAndTrash() async throws {
    let harness = QueueHarness()
    try await harness.launch()
    let kept = try await harness.add("Remove.Keep")
    let trashed = try await harness.add("Remove.Trash")
    #expect(await eventually { harness.isFinished(kept) && harness.isFinished(trashed) })
    let keptFolder = try #require(harness.queue.item(kept)?.outputDirectory)
    let trashedFolder = try #require(harness.queue.item(trashed)?.outputDirectory)

    harness.queue.remove(kept)
    #expect(harness.queue.item(kept) == nil)
    #expect(FileManager.default.fileExists(atPath: keptFolder.path(percentEncoded: false)))
    #expect(!FileManager.default.fileExists(atPath: harness.storage.nzbURL(for: kept).path(percentEncoded: false)))

    harness.queue.moveToTrash(trashed)
    #expect(harness.queue.item(trashed) == nil)
    #expect(await eventually { !FileManager.default.fileExists(atPath: trashedFolder.path(percentEncoded: false)) })
    #expect(harness.queue.actionError == nil)
  }

  // MARK: Follow-ups

  @Test("Too much missing asks first, and Download Anyway downloads without the scan")
  func downloadAnyway() async throws {
    let harness = QueueHarness()
    try await harness.launch()
    let id = try await harness.add("Show.S01E01.Unrepairable.1080p")
    #expect(await eventually { harness.state(id)?.isUnrepairable == true })
    guard case .needsAttention(.unrepairable(let availability)) = harness.state(id) else { throw CancellationError() }
    #expect(availability.verdict == .unrepairable && availability.missingFraction > 0.08)

    harness.queue.downloadAnyway(id)
    #expect(await eventually { harness.state(id)?.finishedOutcome != nil })
    #expect(harness.state(id)?.finishedOutcome == .completedWithIssues)
    #expect(harness.queue.item(id)?.downloadAnyway == true)
  }

  @Test("An encrypted archive asks for its password; a wrong one asks again, the right one finishes")
  func password() async throws {
    let harness = QueueHarness()
    try await harness.launch()
    let id = try await harness.add("Encrypted.Password.Release")
    #expect(await eventually { harness.state(id)?.isNeedsPassword == true })
    #expect(harness.queue.item(id)?.passwordRejected == false)
    let jobsBefore = harness.engine.startedJobCount

    harness.queue.providePassword(id, password: "wrong guess")
    #expect(await eventually { harness.state(id)?.isNeedsPassword == true && harness.queue.item(id)?.passwordRejected == true })

    harness.queue.providePassword(id, password: "letmein")
    #expect(await eventually { harness.isFinished(id) })
    #expect(harness.queue.item(id)?.passwords == ["wrong guess", "letmein"])
    // Reprocessing, not downloading again.
    #expect(harness.engine.startedJobCount == jobsBefore + 2)
  }

  @Test("A password in the NZB opens the archive without asking")
  func passwordInNZB() async throws {
    let harness = QueueHarness()
    try await harness.launch()
    let id = try await harness.add("Encrypted.Password.With.Meta", password: "fromtheindexer")
    #expect(await eventually { harness.isFinished(id) })
  }

  @Test("A password in the NZB's file name stays out of the title and folder, and opens the archive")
  func passwordInFileName() async throws {
    let harness = QueueHarness()
    try await harness.launch()
    let url = try TestNZB.write(title: "Encrypted.Release", in: harness.inbox, fileName: "Encrypted.Release{{letmein}}.nzb", titled: false)
    guard case .added(let id) = await harness.queue.add(url) else {
      Issue.record("the NZB was not added")
      return
    }
    let item = try #require(harness.queue.item(id))
    #expect(item.title == "Encrypted.Release")
    #expect(item.outputDirectory.lastPathComponent == "Encrypted.Release")
    #expect(item.info?.passwords == ["letmein"])
    #expect(await eventually { harness.isFinished(id) })
    #expect(harness.queue.item(id)?.passwords == [])
  }

  @Test("Not enough space needs attention, with the engine's sentence")
  func diskFull() async throws {
    let harness = QueueHarness()
    try await harness.launch()
    let id = try await harness.add("Huge.DiskFull.Release")
    #expect(await eventually { harness.queue.item(id)?.needsAttention == true })
    guard case .needsAttention(.diskFull(let message)) = harness.state(id) else {
      Issue.record("expected disk full, got \(String(describing: harness.state(id)))")
      return
    }
    #expect(message.contains("more free space"))
    #expect(harness.queue.item(id)?.canRetry == true)
    #expect(!harness.queue.isPaused)
  }

  @Test("Too many missing articles fails the job and the queue carries on")
  func failure() async throws {
    let harness = QueueHarness()
    try await harness.launch()
    let failing = try await harness.add("Will.Fail.1080p")
    let next = try await harness.add("Next.In.Line.1080p")
    // The next download starts while the failing one is still repairing.
    #expect(await eventually { harness.isFinished(next) && harness.queue.item(failing)?.isRunning == false })
    guard case .failed(let message, _) = harness.state(failing) else {
      Issue.record("expected a failure, got \(String(describing: harness.state(failing)))")
      return
    }
    #expect(message.contains("9%"))
  }

  // MARK: Server problems

  @Test("A rejected login pauses the queue instead of failing every job")
  func serverProblemPauses() async throws {
    let harness = QueueHarness()
    harness.engine.serverBehaviour = .rejectsLogin
    try await harness.launch()
    let first = try await harness.add("Server.First")
    let second = try await harness.add("Server.Second")
    #expect(await eventually { harness.queue.serverProblem != nil })
    #expect(harness.queue.serverProblem?.kind == .auth)
    #expect(harness.queue.isPaused)
    try await Task.sleep(for: .milliseconds(50))
    #expect(harness.state(first) == .queued && harness.state(second) == .queued)
    #expect(harness.engine.startedJobCount == 1)

    // Not Now hides the alert; the problem stays for the window's notice,
    // and the waiting rows read Paused.
    harness.queue.dismissServerProblem()
    #expect(harness.queue.serverProblem == nil && harness.queue.isPaused)
    #expect(harness.queue.unresolvedServerProblem?.kind == .auth)
    #expect(try harness.queue.isHeld(#require(harness.queue.item(second))))

    // A change on its own (the user still typing) does not try again.
    harness.engine.serverBehaviour = .healthy
    harness.queue.serverSettingsChanged()
    try await Task.sleep(for: .milliseconds(50))
    #expect(harness.engine.startedJobCount == 1 && harness.queue.isPaused)

    // Finishing with the new settings does.
    harness.queue.serverSettingsCommitted()
    #expect(await eventually { harness.isFinished(first) && harness.isFinished(second) })
    #expect(!harness.queue.isPaused && harness.queue.unresolvedServerProblem == nil)
  }

  @Test("Closing the settings unchanged after a server problem leaves the queue paused; Try Again lifts it")
  func serverProblemTryAgain() async throws {
    let harness = QueueHarness()
    harness.engine.serverBehaviour = .unreachable
    try await harness.launch()
    let id = try await harness.add("Server.Again")
    #expect(await eventually { harness.queue.unresolvedServerProblem != nil })
    harness.queue.serverSettingsCommitted()
    try await Task.sleep(for: .milliseconds(50))
    #expect(harness.queue.isPaused && harness.engine.startedJobCount == 1)

    harness.engine.serverBehaviour = .healthy
    harness.queue.retryServer()
    #expect(harness.queue.unresolvedServerProblem == nil && harness.queue.serverProblem == nil)
    #expect(await eventually { harness.isFinished(id) })
    #expect(!harness.queue.isPaused)
  }

  @Test("Try Again leaves Pause All on when the user had it on")
  func serverProblemKeepsPauseAll() async throws {
    let harness = QueueHarness(startAutomatically: true)
    harness.engine.serverBehaviour = .rejectsLogin
    try await harness.launch()
    harness.queue.pauseAll()
    let started = try await harness.add("Started.By.Hand")
    let waiting = try await harness.add("Held.By.Pause.All")
    harness.queue.start(started)
    #expect(await eventually { harness.queue.unresolvedServerProblem != nil })
    harness.engine.serverBehaviour = .healthy
    harness.queue.retryServer()
    #expect(harness.queue.isPaused)
    #expect(await eventually { harness.isFinished(started) })
    #expect(harness.state(waiting) == .queued)
    #expect(try harness.queue.isHeld(#require(harness.queue.item(waiting))))
  }

  @Test("A login refused with settings since replaced raises no problem: the job starts again with the new ones")
  func staleServerProblem() async throws {
    let harness = QueueHarness(timeScale: 2)
    harness.engine.serverBehaviour = .rejectsLogin
    try await harness.launch()
    let id = try await harness.add("Stale.Login")
    #expect(await eventually { harness.queue.item(id)?.phase == .connecting })
    // New settings reach the engine while the old login is on its way.
    harness.queue.serverSettingsChanged()
    try await Task.sleep(for: .milliseconds(400))
    // The first refusal was set aside; the second, with the new settings, counts.
    #expect(await eventually(timeout: .seconds(20)) { harness.queue.serverProblem != nil })
    #expect(harness.engine.startedJobCount == 2)
  }

  @Test("Cancel or Remove of a job that got nothing takes its empty folder away, and leaves one with files")
  func emptyFoldersGo() async throws {
    let harness = QueueHarness()
    harness.engine.serverBehaviour = .rejectsLogin
    try await harness.launch()
    let cancelled = try await harness.add("Empty.Cancelled")
    let removed = try await harness.add("Empty.Removed")
    let kept = try await harness.add("Empty.Kept")
    #expect(await eventually { harness.queue.serverProblem != nil })
    // The engine makes a job's folder as it starts; here the login failed first.
    let folders = try [cancelled, removed, kept].map { try #require(harness.queue.item($0)?.outputDirectory) }
    for folder in folders { try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true) }
    try Data("note".utf8).write(to: folders[2].appending(path: "note.txt"))

    harness.queue.stop(cancelled)
    harness.queue.remove(removed)
    harness.queue.remove(kept)
    #expect(await eventually { !FileManager.default.fileExists(atPath: folders[0].path(percentEncoded: false)) })
    #expect(await eventually { !FileManager.default.fileExists(atPath: folders[1].path(percentEncoded: false)) })
    try await Task.sleep(for: .milliseconds(100))
    #expect(FileManager.default.fileExists(atPath: folders[2].appending(path: "note.txt").path(percentEncoded: false)))
  }

  @Test("No server, no downloads: items wait until one is configured")
  func waitsForAServer() async throws {
    let harness = QueueHarness()
    harness.settings.host = ""
    try await harness.launch()
    let id = try await harness.add("No.Server.Yet")
    try await Task.sleep(for: .milliseconds(50))
    #expect(harness.state(id) == .queued)
    harness.settings.host = "news.example.com"
    #expect(await eventually { harness.isFinished(id) })
  }

  // MARK: Persistence

  @Test("The list survives a relaunch exactly")
  func persistenceRoundTrip() async throws {
    let harness = QueueHarness(startAutomatically: false)
    try await harness.launch()
    _ = try await harness.add("Saved.One")
    _ = try await harness.add("Saved.Two")
    harness.queue.saveNow()

    let relaunched = DownloadQueue(engine: SimulatedEngine(configuration: .fast()), settings: harness.settings, storage: harness.storage)
    relaunched.restore()
    #expect(relaunched.items == harness.queue.items)
  }

  @Test("A job running at quit comes back queued and continues from its folder")
  func restoreAndResume() async throws {
    let harness = QueueHarness(timeScale: 250)
    harness.engine.scenarioOverride = .normal
    try await harness.launch()
    await harness.throttle()
    let id = try await harness.add("Interrupted.Release")
    #expect(await harness.waitUntilDownloading(id))
    await harness.queue.prepareForQuit()
    let saved = harness.storage.load()
    let savedItem = try #require(saved.items.first { $0.id == id })
    #expect(savedItem.state == .queued)
    let bytesBefore = try #require(savedItem.progress?.bytesDone)
    #expect(bytesBefore > 0)
    let firstRun = try #require(savedItem.earlierRunSeconds)
    #expect(firstRun > 0)

    let engine = SimulatedEngine(configuration: .fast(timeScale: 250))
    engine.scenarioOverride = .normal
    let relaunched = QueueHarness(storage: harness.storage, engine: engine)
    try await relaunched.launch()
    #expect(relaunched.queue.items.map(\.id) == [id])
    #expect(await eventually { relaunched.isFinished(id) })
    #expect(engine.startedJobCount == 1)
    // "Finished in" counts both runs, not just the one after the relaunch.
    let finished = try #require(relaunched.queue.item(id))
    #expect(finished.earlierRunSeconds == nil)
    #expect((finished.summary?.elapsedSeconds ?? 0) > firstRun)
  }

  @Test("With automatic start off, an interrupted job waits for Start after a relaunch")
  func restoreWaitsForStart() async throws {
    let storage = QueueStorage.temporary()
    defer { try? FileManager.default.removeItem(at: storage.directory) }
    var item = PreviewData.downloading
    item.nzbURL = try TestNZB.write(title: "Waiting.After.Relaunch", in: storage.nzbDirectory)
    item.startRequested = true
    try storage.save(QueueStorage.Snapshot(items: [item]))

    let harness = QueueHarness(startAutomatically: false, storage: storage)
    try await harness.launch()
    #expect(harness.queue.items.first?.state == .queued)
    #expect(harness.queue.items.first?.startRequested == false)
    try await Task.sleep(for: .milliseconds(50))
    #expect(harness.engine.startedJobCount == 0)
  }

  @Test("Pause All is remembered across a relaunch")
  func pausedByUserPersists() async throws {
    let harness = QueueHarness(startAutomatically: false)
    try await harness.launch()
    _ = try await harness.add("Held.Release")
    harness.queue.pauseAll()
    harness.queue.saveNow()
    let relaunched = DownloadQueue(engine: SimulatedEngine(configuration: .fast()), settings: harness.settings, storage: harness.storage)
    relaunched.restore()
    #expect(relaunched.isPaused)
  }

  @Test("An unreadable queue.json is set aside, not overwritten")
  func unreadableQueueFile() throws {
    let storage = QueueStorage.temporary()
    defer { try? FileManager.default.removeItem(at: storage.directory) }
    try FileManager.default.createDirectory(at: storage.directory, withIntermediateDirectories: true)
    try Data("{ not json".utf8).write(to: storage.queueFile)
    #expect(storage.load().items.isEmpty)
    let names = try FileManager.default.contentsOfDirectory(atPath: storage.directory.path(percentEncoded: false))
    #expect(names.contains { $0.hasPrefix("queue-unreadable-") })
  }

  // MARK: Retention

  @Test("After one day, finished items leave the list and their files stay")
  func retentionAfterOneDay() async throws {
    let harness = QueueHarness()
    harness.settings.retention = .afterOneDay
    var clock = Date(timeIntervalSinceReferenceDate: 800_000_000)
    harness.queue.now = { clock }
    try await harness.launch()
    let id = try await harness.add("Retained.Release")
    #expect(await eventually { harness.isFinished(id) })
    let folder = try #require(harness.queue.item(id)?.outputDirectory)

    clock = clock.addingTimeInterval(23 * 3_600)
    harness.queue.applyRetention()
    #expect(harness.queue.item(id) != nil)
    clock = clock.addingTimeInterval(2 * 3_600)
    harness.queue.applyRetention()
    #expect(harness.queue.item(id) == nil)
    #expect(FileManager.default.fileExists(atPath: folder.path(percentEncoded: false)))
  }

  @Test("When dl-nzb quits, finished items leave; unfinished ones stay")
  func retentionWhenQuitting() async throws {
    let harness = QueueHarness()
    harness.settings.retention = .whenAppQuits
    try await harness.launch()
    let finished = try await harness.add("Quit.Finished")
    #expect(await eventually { harness.isFinished(finished) })
    harness.settings.startAutomatically = false
    let waiting = try await harness.add("Quit.Waiting")
    await harness.queue.prepareForQuit()
    #expect(harness.queue.item(finished) == nil)
    #expect(harness.queue.item(waiting) != nil)
    #expect(harness.storage.load().items.map(\.id) == [waiting])
  }

  @Test("Manually keeps everything")
  func retentionManually() async throws {
    let harness = QueueHarness()
    harness.queue.now = { Date.distantFuture }
    try await harness.launch()
    let id = try await harness.add("Kept.Release")
    #expect(await eventually { harness.isFinished(id) })
    harness.queue.applyRetention()
    await harness.queue.prepareForQuit()
    #expect(harness.queue.item(id) != nil)
  }

  // MARK: Aggregates

  @Test("Speed, fraction and counts add up across the list")
  func aggregates() {
    let queue = DownloadQueue.preview(items: [PreviewData.downloading, PreviewData.extracting, PreviewData.queued, PreviewData.paused, PreviewData.finished])
    #expect(queue.activeCount == 2)
    #expect(queue.items.count(where: \.usesNetwork) == 1)
    #expect(queue.queuedCount == 1)
    #expect(queue.pausedCount == 1)
    #expect(queue.unfinishedCount == 4)
    #expect(queue.speed() == 84_400_000)
    #expect(queue.currentItem?.id == PreviewData.downloading.id)
    // Downloading 38% of 8.6 GB, extracting 5.1 GB (downloaded), waiting 8.6 GB, paused at 41% of 5.1 GB.
    #expect((0.35...0.41).contains(queue.overallFraction ?? -1))
    #expect(DownloadQueue.preview(items: [PreviewData.finished, PreviewData.queued]).overallFraction == nil)
  }

  // MARK: Progress and redrawing

  @Test("Progress reaches the item without redrawing what reads the list; a new phase does")
  func progressLeavesTheListAlone() throws {
    let queue = DownloadQueue.preview(items: [PreviewData.downloading, PreviewData.queued])
    let id = PreviewData.downloading.id
    var progress = try #require(PreviewData.downloading.progress)
    let listChanges = ChangeCount()
    withObservationTracking {
      _ = queue.items
    } onChange: {
      listChanges.increment()
    }

    progress.bytesDone += 1_000_000
    queue.apply(.progress(progress), to: id)
    #expect(listChanges.value == 0)
    #expect(queue.item(id)?.progress == progress)

    progress.phase = .verifying
    queue.apply(.progress(progress), to: id)
    #expect(listChanges.value == 1)
    #expect(queue.item(id)?.state == .running(.verifying))
  }

  @Test("What shows an item hears its numbers at most once an interval, and the latest last")
  func liveItemHearsProgress() async throws {
    let queue = DownloadQueue.preview(items: [PreviewData.downloading])
    queue.progressInterval = .milliseconds(200)
    let item = PreviewData.downloading
    var progress = try #require(item.progress)
    let heard = ChangeCount()
    func watch() {
      withObservationTracking {
        _ = queue.live(item)
      } onChange: {
        heard.increment()
      }
    }

    watch()
    progress.bytesDone += 1
    queue.apply(.progress(progress), to: item.id)
    #expect(heard.value == 1)

    watch()
    progress.bytesDone += 1
    queue.apply(.progress(progress), to: item.id)
    progress.bytesDone += 1
    queue.apply(.progress(progress), to: item.id)
    #expect(heard.value == 1)
    #expect(await eventually { heard.value == 2 })
    #expect(queue.live(item).progress == progress)
  }

  @Test("The speed and the overall fraction follow every transfer's numbers")
  func aggregatesHearProgress() throws {
    let queue = DownloadQueue.preview(items: [PreviewData.downloading, PreviewData.extracting])
    queue.progressInterval = .zero
    var progress = try #require(PreviewData.downloading.progress)
    let heard = ChangeCount()
    withObservationTracking {
      _ = queue.speed()
      _ = queue.overallFraction
    } onChange: {
      heard.increment()
    }

    progress.bytesDone += 1_000_000
    queue.apply(.progress(progress), to: PreviewData.downloading.id)
    #expect(heard.value == 1)
  }

  @Test("Post-processing numbers do not wake the speed or the overall fraction, which they cannot change")
  func aggregatesIgnorePostProcessing() throws {
    let queue = DownloadQueue.preview(items: [PreviewData.downloading, PreviewData.extracting])
    queue.progressInterval = .zero
    var progress = try #require(PreviewData.extracting.progress)
    let heard = ChangeCount()
    withObservationTracking {
      _ = queue.speed()
      _ = queue.overallFraction
    } onChange: {
      heard.increment()
    }

    progress.fraction += 0.1
    queue.apply(.progress(progress), to: PreviewData.extracting.id)
    #expect(heard.value == 0)
    #expect(queue.item(PreviewData.extracting.id)?.progress == progress)
  }

  @Test("Pause All is offered only while something can pause, Resume All only while something is held")
  func pauseAndResumeAllAvailability() {
    let running = DownloadQueue.preview(items: [PreviewData.downloading, PreviewData.finished])
    #expect(running.canPauseAll && !running.canResumeAll)
    #expect(DownloadQueue.preview(items: [PreviewData.queued]).canPauseAll)

    let done = DownloadQueue.preview(items: [PreviewData.finished, PreviewData.failed])
    #expect(!done.canPauseAll && !done.canResumeAll)
    #expect(!DownloadQueue.preview(items: []).canPauseAll)
    // Post-processing cannot pause.
    #expect(!DownloadQueue.preview(items: [PreviewData.extracting]).canPauseAll)

    let paused = DownloadQueue.preview(items: [PreviewData.paused, PreviewData.finished])
    #expect(!paused.canPauseAll && paused.canResumeAll)
    let held = DownloadQueue.preview(items: [PreviewData.queued], isPaused: true)
    #expect(!held.canPauseAll && held.canResumeAll)
  }

  @Test("Preview queues ignore every action")
  func previewIsInert() async {
    let queue = DownloadQueue.preview()
    let before = queue.items
    queue.pauseAll()
    queue.remove(PreviewData.downloading.id)
    queue.move(fromOffsets: [0], toOffset: 3)
    #expect(await queue.add(URL(filePath: "/nonexistent.nzb")) == .failed(fileName: "nonexistent.nzb", message: "Downloads are not available here."))
    #expect(queue.items == before && !queue.isPaused)
  }

  @Test("Reordering moves items as SwiftUI's onMove expects")
  func moveElements() {
    var letters = ["a", "b", "c", "d"]
    letters.moveElements(fromOffsets: [0], toOffset: 4)
    #expect(letters == ["b", "c", "d", "a"])
    letters.moveElements(fromOffsets: [1, 2], toOffset: 0)
    #expect(letters == ["c", "d", "b", "a"])
    letters.moveElements(fromOffsets: [3], toOffset: 1)
    #expect(letters == ["c", "a", "d", "b"])
  }
}

/// The composition root: launching, engine choice and settings reaching the engine.
@MainActor
@Suite("App model")
struct AppModelTests {
  @Test("-simulate YES asks for the simulated engine")
  func engineChoice() throws {
    let suite = "com.zephleggett.dl-nzb.tests.\(UUID().uuidString)"
    let defaults = try #require(UserDefaults(suiteName: suite))
    defer { defaults.removePersistentDomain(forName: suite) }
    #expect(!AppModel.simulateRequested(defaults: defaults))
    defaults.set("YES", forKey: "simulate")
    #expect(AppModel.simulateRequested(defaults: defaults))

    var asked: [AppModel.EngineKind] = []
    let model = AppModel(settings: .preview(), storage: .temporary(), simulate: true) { kind in
      asked.append(kind)
      return SimulatedEngine()
    }
    #expect(asked == [.simulated] && model.engineKind == .simulated)
  }

  @Test("Launch is idempotent, and settings changes reach the engine")
  func launchAndSettings() async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast())
    let settings = SettingsStore.preview(downloadFolder: scratch.folder("Downloads"))
    let model = AppModel(settings: settings, storage: QueueStorage(directory: scratch.folder("Support")), simulate: true) { _ in engine }
    model.launch()
    model.launch()
    #expect(model.isLaunched)
    #expect(await eventually { engine.settings.server.host == "news.example.com" })
    #expect(engine.settings.password == "secret")

    settings.connections = 8
    settings.limitsSpeed = true
    settings.speedLimitMegabytesPerSecond = 5
    #expect(await eventually { engine.settings.server.connections == 8 && engine.speedLimit == 5_000_000 })

    settings.connections = 0
    #expect(await eventually { model.settingsProblem != nil })
    settings.connections = 10
    #expect(await eventually { model.settingsProblem == nil })

    let check = try await model.testConnection()
    #expect(check.greeting.contains("news.example.com"))
    await model.prepareForQuit()
  }

  @Test("A new password tries again only once the user has finished with it, or Test Connection worked")
  func serverSettingsCommitted() async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast(), serverBehaviour: .rejectsLogin)
    let settings = SettingsStore.preview(downloadFolder: scratch.folder("Downloads"))
    let model = AppModel(settings: settings, storage: QueueStorage(directory: scratch.folder("Support")), simulate: true) { _ in engine }
    model.launch()
    let url = try TestNZB.write(title: "New.Login.Release", in: scratch.folder("Inbox"))
    guard case .added(let id) = await model.open([url]).first else { throw CancellationError() }
    #expect(await eventually { model.queue.unresolvedServerProblem != nil })

    // Typing a new password reaches the engine but does not try it yet.
    engine.serverBehaviour = .healthy
    settings.password = "corrected"
    #expect(await eventually { engine.settings.password == "corrected" })
    try await Task.sleep(for: .milliseconds(100))
    #expect(model.queue.isPaused && engine.startedJobCount == 1)

    model.serverSettingsCommitted()
    #expect(await eventually { model.queue.item(id)?.isFinished == true })
    #expect(model.queue.unresolvedServerProblem == nil)
    await model.prepareForQuit()
  }

  @Test("Test Connection that works lifts a server problem")
  func testConnectionRetries() async throws {
    let scratch = Scratch()
    let engine = SimulatedEngine(configuration: .fast(), serverBehaviour: .unreachable)
    let settings = SettingsStore.preview(downloadFolder: scratch.folder("Downloads"))
    let model = AppModel(settings: settings, storage: QueueStorage(directory: scratch.folder("Support")), simulate: true) { _ in engine }
    let url = try TestNZB.write(title: "Back.Online", in: scratch.folder("Inbox"))
    guard case .added(let id) = await model.open([url]).first else { throw CancellationError() }
    #expect(await eventually { model.queue.unresolvedServerProblem != nil })
    engine.serverBehaviour = .healthy
    _ = try await model.testConnection()
    #expect(await eventually { model.queue.item(id)?.isFinished == true })
    await model.prepareForQuit()
  }

  @Test("Opening before launch launches first")
  func openLaunches() async throws {
    let scratch = Scratch()
    let model = AppModel(
      settings: .preview(downloadFolder: scratch.folder("Downloads")), storage: QueueStorage(directory: scratch.folder("Support")), simulate: true
    ) { _ in SimulatedEngine(configuration: .fast()) }
    let url = try TestNZB.write(title: "Opened.From.Finder", in: scratch.folder("Inbox"))
    let results = await model.open([url])
    #expect(model.isLaunched)
    guard case .added(let id) = results.first else {
      Issue.record("expected the NZB to be added, got \(results)")
      return
    }
    #expect(await eventually { model.queue.item(id)?.isFinished == true })
  }

  @Test("The preview model has every sample and does nothing")
  func preview() {
    let model = AppModel.preview()
    model.launch()
    #expect(!model.isLaunched)
    #expect(model.queue.items == PreviewData.items)
  }
}

/// Counts observation callbacks, which come on whatever thread made the change.
final class ChangeCount: Sendable {
  private let count = Mutex(0)

  var value: Int { count.withLock { $0 } }

  func increment() {
    count.withLock { $0 += 1 }
  }
}
