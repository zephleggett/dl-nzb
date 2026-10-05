import DlNzbKit
import Foundation

/// Keeps downloads going when the user leaves the app.
///
/// iOS 26's `BGContinuedProcessingTask` is made for this: work the user
/// started in the foreground carries on in the background while the system
/// shows its progress in a Live Activity. One task covers a whole queue run:
///
/// 1. The user starts something (adds an NZB, Resume, Retry, Start) while
///    the app is in front and no task is pending or running. The runner
///    registers a handler for a fresh identifier,
///    `<bundle id>.download.<UUID>`, and submits the request. Identifiers are
///    never reused: registering one twice kills the app, and the system says
///    nothing about when it is done with one.
/// 2. The handler starts the run: every second it reports the run's progress
///    in bytes (`RunProgress`) and keeps the title and subtitle on the item
///    in hand, so the system never sees a stalled task.
/// 3. The run ends when nothing is running any more: the task completes.
/// 4. If the system (or the user, from the Live Activity) ends the task early,
///    the queue pauses cleanly, keeping its data, and resumes when the app
///    comes back to the front.
///
/// Leaving the app without a running task (none was needed, the request is
/// still queued, or the simulator, which has no continued processing) falls
/// back on `beginBackgroundTask`: a short grace period, after which the queue
/// pauses the same way.
///
/// Either way, a pause in the background is announced once, by
/// `onPausedInBackground`, so it is not found out hours later.
///
/// Not observable: no view shows its state.
@MainActor
final class BackgroundRunner {
  enum Phase: Equatable {
    case idle
    /// Submitted; the system has not started it yet.
    case pending(String)
    case running(String)
  }

  private(set) var phase: Phase = .idle
  /// In front of the user: only then may a task be submitted.
  private(set) var isForeground = true
  /// The queue was paused because background time ran out: the app tells
  /// the user, who would otherwise find it still paused much later.
  var onPausedInBackground: (@MainActor () -> Void)?

  private let queue: any QueueControlling
  private let holds: QueueHolds
  private let scheduler: any ContinuedTaskScheduling
  private let backgroundTime: any BackgroundTimeProviding
  private let identifierPrefix: String
  private let makeSuffix: @MainActor () -> String
  private var task: (any ContinuedTask)?
  /// Items that belong to this run: everything that ran or waited since it
  /// started. Finished ones stay in, so the bar never goes backwards.
  private var runIDs: Set<DownloadItem.ID> = []
  private var shown: (title: String, subtitle: String)?
  private var ticker: Task<Void, Never>?
  private var graceToken: Int?

  init(
    queue: any QueueControlling,
    holds: QueueHolds,
    scheduler: any ContinuedTaskScheduling,
    backgroundTime: any BackgroundTimeProviding,
    bundleIdentifier: String = Bundle.main.bundleIdentifier ?? "com.zephleggett.dl-nzb",
    makeSuffix: @escaping @MainActor () -> String = { UUID().uuidString }
  ) {
    self.queue = queue
    self.holds = holds
    self.scheduler = scheduler
    self.backgroundTime = backgroundTime
    self.identifierPrefix = "\(bundleIdentifier).download."
    self.makeSuffix = makeSuffix
  }

  /// Whether leaving the app now keeps the downloads going.
  var isCovered: Bool {
    if case .running = phase { return true }
    return false
  }

  var hasGracePeriod: Bool { graceToken != nil }

  // MARK: Events

  /// The user just started or resumed something. Submits a task for the run
  /// unless one is pending or running already.
  func userStartedWork() {
    guard isForeground, phase == .idle, queue.activeCount > 0 else { return }
    let identifier = identifierPrefix + makeSuffix()
    let registered = scheduler.register(identifier) { [weak self] task in
      self?.began(task, identifier: identifier)
    }
    guard registered else {
      AppLog.background.error("the system did not accept the task identifier \(identifier, privacy: .public)")
      return
    }
    let text = currentText
    do {
      try scheduler.submit(ContinuedTaskRequest(identifier: identifier, title: text.title, subtitle: text.subtitle))
      phase = .pending(identifier)
      runIDs = []
      absorbRunItems()
      startTicker()
      AppLog.background.info("submitted a continued processing task for the queue run")
    } catch {
      // The simulator has none, the user may have turned background work
      // off, or the system is busy. The grace period still applies.
      let code = (error as NSError).code
      AppLog.background.notice("the continued processing task was not submitted (code \(code)): \(error.localizedDescription, privacy: .public)")
    }
  }

  func sceneDidEnterBackground() {
    isForeground = false
    queue.saveNow()
    guard !isCovered, queue.activeCount > 0, graceToken == nil else { return }
    graceToken = backgroundTime.begin(name: "Finish downloads") { [weak self] in
      self?.graceExpired()
    }
    AppLog.background.info("in the background without a continued task; using the grace period")
    startTicker()
  }

  func sceneDidBecomeActive() {
    isForeground = true
    endGracePeriod()
    if holds.isHolding(.backgroundExpired) {
      AppLog.background.info("back in front; resuming what the background expiry paused")
      holds.release(.backgroundExpired)
    }
  }

  // MARK: The task

  private func began(_ task: any ContinuedTask, identifier: String) {
    guard phase == .pending(identifier) else {
      // A request from an earlier run that started late: nothing to cover.
      task.complete(success: true)
      return
    }
    self.task = task
    phase = .running(identifier)
    shown = nil
    task.setExpirationHandler { [weak self] in
      self?.expired(identifier)
    }
    // The task covers the background now; the grace period is not needed.
    endGracePeriod()
    AppLog.background.info("the continued processing task started")
    tick()
    startTicker()
  }

  /// Reports progress, follows the current item, and ends the run once
  /// nothing is running. Every second while a task runs or the grace period lasts.
  func tick() {
    // Nothing running means the run is over: the queue starts the next job
    // in the same turn the last one finishes, so there is no gap between them.
    let running = queue.activeCount
    if graceToken != nil, running == 0 {
      // Nothing left to finish: let the app suspend now rather than later.
      endGracePeriod()
    }
    if case .pending(let identifier) = phase, running == 0 {
      // The run ended before the system got round to it. Should it start
      // after all, `began` completes it at once.
      scheduler.cancel(identifier)
      phase = .idle
      runIDs = []
      AppLog.background.info("the queue run ended before its task started")
    }
    guard case .running = phase, let task else {
      if graceToken == nil, phase == .idle { stopTicker() }
      return
    }
    absorbRunItems()
    guard running > 0 else {
      finish(success: true)
      return
    }
    let progress = RunProgress.of(queue.items.filter { runIDs.contains($0.id) })
    task.progress.totalUnitCount = max(progress.total, 1)
    task.progress.completedUnitCount = progress.completed
    let text = currentText
    if shown?.title != text.title || shown?.subtitle != text.subtitle {
      task.update(title: text.title, subtitle: text.subtitle)
      shown = text
    }
  }

  private func expired(_ identifier: String) {
    guard phase == .running(identifier) else { return }
    if isForeground {
      // Ended while the app is in front (the user tapped stop in the system
      // UI): the downloads carry on in the app.
      AppLog.background.info("the continued processing task ended while in front; downloads continue")
    } else {
      AppLog.background.notice("the continued processing task expired; pausing the queue")
      holds.hold(.backgroundExpired)
      queue.saveNow()
      onPausedInBackground?()
    }
    // Success: the run paused cleanly and continues later. Failure would
    // put up the system's failure UI for something that is not one.
    finish(success: true)
  }

  private func finish(success: Bool) {
    task?.complete(success: success)
    task = nil
    phase = .idle
    runIDs = []
    shown = nil
    if graceToken == nil { stopTicker() }
    AppLog.background.info("the queue run is over")
  }

  // MARK: Grace period

  private func graceExpired() {
    guard let token = graceToken else { return }
    graceToken = nil
    if !isCovered, queue.activeCount > 0 {
      AppLog.background.notice("background time is up; pausing the queue")
      holds.hold(.backgroundExpired)
      queue.saveNow()
      onPausedInBackground?()
    }
    backgroundTime.end(token)
    if phase == .idle { stopTicker() }
  }

  private func endGracePeriod() {
    guard let token = graceToken else { return }
    graceToken = nil
    backgroundTime.end(token)
  }

  // MARK: Helpers

  /// The Live Activity's title and subtitle, for the item in hand and the
  /// downloads waiting behind it.
  private var currentText: (title: String, subtitle: String) {
    let current = queue.currentItem
    let waiting = queue.items.count(where: \.isQueued)
    return (ContinuedTaskText.title(for: current), ContinuedTaskText.subtitle(for: current, waiting: waiting))
  }

  /// Takes in whatever is running or waiting to run. Items that leave the
  /// list drop out of the progress with it.
  private func absorbRunItems() {
    for item in queue.items where item.isRunning || item.isQueued {
      runIDs.insert(item.id)
    }
  }

  private func startTicker() {
    guard ticker == nil else { return }
    ticker = Task { [weak self] in
      while true {
        try? await Task.sleep(for: .seconds(1))
        guard !Task.isCancelled, let self else { return }
        self.tick()
      }
    }
  }

  private func stopTicker() {
    ticker?.cancel()
    ticker = nil
  }
}
