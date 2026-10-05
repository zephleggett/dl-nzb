import DlNzbKit
import Foundation
import Testing

@testable import DlNzbApp

@MainActor
@Suite("Background runner")
struct BackgroundRunnerTests {
  let queue: FakeQueue
  let holds: QueueHolds
  let scheduler = FakeScheduler()
  let time = FakeBackgroundTime()
  let runner: BackgroundRunner
  private let suffixes: SuffixSource

  init() {
    queue = FakeQueue([Items.downloading(1, done: 2_000, of: 8_000), Items.make(2, .queued, bytes: 2_000)])
    holds = QueueHolds(queue: queue, defaults: scratchDefaults())
    let suffixes = SuffixSource()
    self.suffixes = suffixes
    runner = BackgroundRunner(
      queue: queue, holds: holds, scheduler: scheduler, backgroundTime: time, bundleIdentifier: "com.example.dl-nzb", makeSuffix: { suffixes.next() })
  }

  @Test("Starting work in front submits one task, under a fresh identifier with the bundle's prefix")
  func submitsOnce() {
    runner.userStartedWork()
    runner.userStartedWork()
    #expect(scheduler.submitted.count == 1)
    #expect(scheduler.submitted.first?.identifier == "com.example.dl-nzb.download.1")
    #expect(scheduler.submitted.first?.title == "Release 1")
    #expect(scheduler.submitted.first?.subtitle == "2 of 8 kB · 1 waiting")
    #expect(runner.phase == .pending("com.example.dl-nzb.download.1"))
  }

  @Test("Nothing is submitted from the background, or when nothing is running")
  func submitsOnlyWhenItShould() {
    runner.sceneDidEnterBackground()
    runner.userStartedWork()
    #expect(scheduler.submitted.isEmpty)

    runner.sceneDidBecomeActive()
    queue.items = [Items.make(3, .queued)]
    runner.userStartedWork()
    #expect(scheduler.submitted.isEmpty)
  }

  @Test("A running task reports the run's bytes and follows the current item")
  func reportsProgress() {
    runner.userStartedWork()
    let task = scheduler.start()
    #expect(runner.isCovered)
    #expect(task.progress.totalUnitCount == 10_000)
    // 2,000 of 8,000 downloaded is a quarter of the download share.
    #expect(task.progress.completedUnitCount == Int64(8_000 * 0.25 * RunProgress.downloadShare))
    #expect(task.title == "Release 1")

    queue.update(Items.downloading(1, done: 0, of: 0).id) { $0.state = .running(.repairing) }
    queue.update(Items.downloading(1, done: 0, of: 0).id) { $0.progress = JobProgress(phase: .repairing, fraction: 0.5, damagedBlocks: 12) }
    runner.tick()
    #expect(task.title == "Release 1")
    #expect(task.subtitle == "Repairing 12 damaged blocks · 50% · 1 waiting")
    #expect(task.completed == nil)
  }

  @Test("The task completes once nothing runs, and the next start submits a new one")
  func completesWhenIdle() {
    runner.userStartedWork()
    let task = scheduler.start()
    queue.items = queue.items.map { item in
      var item = item
      item.state = .finished(JobSummary(outcome: .completed, outputDirectory: item.outputDirectory))
      return item
    }
    runner.tick()
    #expect(task.completed == true)
    #expect(runner.phase == .idle)

    queue.items.append(Items.downloading(4, done: 1, of: 10))
    runner.userStartedWork()
    #expect(scheduler.submitted.map(\.identifier) == ["com.example.dl-nzb.download.1", "com.example.dl-nzb.download.2"])
  }

  @Test("Expiry in the background pauses the queue cleanly; coming back resumes it")
  func expiryPausesAndResumes() {
    queue.items.append(Items.make(5, .paused))
    runner.userStartedWork()
    let task = scheduler.start()
    runner.sceneDidEnterBackground()
    task.expire()
    #expect(task.completed == true)
    #expect(holds.isHolding(.backgroundExpired))
    #expect(queue.isPaused)
    #expect(queue.state(Items.make(1, .queued).id) == .paused)
    #expect(queue.saves >= 2)

    runner.sceneDidBecomeActive()
    #expect(!holds.isHolding)
    #expect(!queue.isPaused)
    #expect(queue.state(Items.make(1, .queued).id) == .queued)
    // Paused by the user before: still paused.
    #expect(queue.state(Items.make(5, .queued).id) == .paused)
  }

  @Test("Ended while in front, the downloads carry on, and nothing is announced")
  func expiryInFront() {
    var announced = 0
    runner.onPausedInBackground = { announced += 1 }
    runner.userStartedWork()
    let task = scheduler.start()
    task.expire()
    #expect(task.completed == true)
    #expect(!holds.isHolding)
    #expect(!queue.isPaused)
    #expect(announced == 0)
  }

  @Test("A pause in the background is announced once, whether the task or the grace period ran out")
  func announcesPause() {
    var announced = 0
    runner.onPausedInBackground = { announced += 1 }
    runner.userStartedWork()
    let task = scheduler.start()
    runner.sceneDidEnterBackground()
    task.expire()
    #expect(announced == 1)

    // Back in front, the queue runs again; this time no task is granted.
    runner.sceneDidBecomeActive()
    queue.items = [Items.downloading(7, done: 1_000, of: 8_000)]
    scheduler.failSubmission = true
    runner.userStartedWork()
    runner.sceneDidEnterBackground()
    time.expireAll()
    #expect(announced == 2)

    // Nothing running when the time runs out: nothing paused, nothing said.
    runner.sceneDidBecomeActive()
    queue.items = [Items.make(6, .finished(JobSummary(outcome: .completed, outputDirectory: URL(filePath: "/tmp"))))]
    runner.sceneDidEnterBackground()
    time.expireAll()
    #expect(announced == 2)
  }

  @Test("Without a task, leaving uses the grace period, whose end pauses the queue")
  func gracePeriod() {
    scheduler.failSubmission = true
    runner.userStartedWork()
    runner.sceneDidEnterBackground()
    #expect(runner.hasGracePeriod)
    time.expireAll()
    #expect(holds.isHolding(.backgroundExpired))
    #expect(time.ended == [1])
    runner.sceneDidBecomeActive()
    #expect(!queue.isPaused)
  }

  @Test("A task that starts during the grace period takes over from it")
  func taskReplacesGracePeriod() {
    runner.userStartedWork()
    runner.sceneDidEnterBackground()
    #expect(runner.hasGracePeriod)
    scheduler.start()
    #expect(!runner.hasGracePeriod)
    #expect(time.ended == [1])
  }

  @Test("A request still queued when the run ends is withdrawn, and completes at once if it starts late")
  func pendingRunEnds() {
    runner.userStartedWork()
    queue.items = []
    runner.tick()
    #expect(scheduler.cancelled == ["com.example.dl-nzb.download.1"])
    #expect(runner.phase == .idle)
    let late = scheduler.start("com.example.dl-nzb.download.1")
    #expect(late.completed == true)
    #expect(!runner.isCovered)
  }
}

/// "1", "2", … so identifiers are predictable and visibly never reused.
@MainActor
final class SuffixSource {
  private var count = 0

  func next() -> String {
    count += 1
    return "\(count)"
  }
}
