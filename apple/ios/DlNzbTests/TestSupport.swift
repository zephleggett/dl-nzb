import DlNzbKit
import Foundation
import Testing

@testable import DlNzbApp

/// A queue that does what `DownloadQueue` does to item states, with no
/// engine, and remembers what it was asked.
@MainActor
final class FakeQueue: QueueControlling {
  var items: [DownloadItem]
  var isPaused = false
  private(set) var calls: [String] = []
  private(set) var saves = 0

  init(_ items: [DownloadItem] = []) {
    self.items = items
  }

  // As `DownloadQueue` defines them.
  var activeCount: Int { items.count(where: \.isRunning) }
  var currentItem: DownloadItem? { items.first(where: \.usesNetwork) ?? items.first(where: \.isRunning) }
  /// Downloads start automatically here.
  var hasNetworkWork: Bool { items.contains { $0.usesNetwork || $0.mayStart(automatically: !isPaused) } }

  func pause(_ id: DownloadItem.ID) {
    calls.append("pause")
    update(id) { $0.state = .paused }
  }

  func resume(_ id: DownloadItem.ID) {
    calls.append("resume")
    update(id) { $0.state = .queued }
  }

  func pauseAll() {
    calls.append("pauseAll")
    isPaused = true
    for index in items.indices where items[index].usesNetwork {
      items[index].state = .paused
    }
  }

  func resumeAll(keepingPaused kept: Set<DownloadItem.ID>) {
    calls.append("resumeAll")
    isPaused = false
    for index in items.indices where items[index].isPaused && !kept.contains(items[index].id) {
      items[index].state = .queued
    }
  }

  func saveNow() {
    saves += 1
  }

  func update(_ id: DownloadItem.ID, _ change: (inout DownloadItem) -> Void) {
    guard let index = items.firstIndex(where: { $0.id == id }) else { return }
    change(&items[index])
  }

  func state(_ id: DownloadItem.ID) -> DownloadItem.State? {
    items.first { $0.id == id }?.state
  }
}

/// Builds items in a given state, sized in bytes.
enum Items {
  static func make(_ number: Int, _ state: DownloadItem.State, bytes: Int64 = 1_000, progress: JobProgress? = nil, title: String? = nil)
    -> DownloadItem
  {
    let id = UUID(uuidString: String(format: "00000000-0000-0000-0000-%012d", number)) ?? UUID()
    let name = title ?? "Release.\(number)"
    return DownloadItem(
      id: id, title: name, nzbURL: URL(filePath: "/tmp/\(number).nzb"), originalFileName: "\(name).nzb", fingerprint: "\(number)",
      outputDirectory: URL(filePath: "/tmp/Downloads/\(name)", directoryHint: .isDirectory),
      info: NzbInfo(title: name, totalBytes: bytes, dataBytes: bytes, par2Bytes: 0), state: state, progress: progress)
  }

  static func downloading(_ number: Int, done: Int64, of total: Int64, title: String? = nil) -> DownloadItem {
    make(
      number, .running(.downloading), bytes: total,
      progress: JobProgress(phase: .downloading, bytesDone: done, bytesTotal: total, speedBytesPerSecond: 80_000_000, etaSeconds: 30), title: title)
  }
}

/// A scheduler that records what was registered and submitted, and starts
/// tasks when the test says so.
@MainActor
final class FakeScheduler: ContinuedTaskScheduling {
  var handlers: [String: (any ContinuedTask) -> Void] = [:]
  var submitted: [ContinuedTaskRequest] = []
  var cancelled: [String] = []
  var registrations: [String] = []
  var failSubmission = false

  func register(_ identifier: String, launchHandler: @escaping @MainActor (any ContinuedTask) -> Void) -> Bool {
    registrations.append(identifier)
    guard handlers[identifier] == nil else {
      Issue.record("registered \(identifier) twice, which kills the app")
      return false
    }
    handlers[identifier] = launchHandler
    return true
  }

  func submit(_ request: ContinuedTaskRequest) throws {
    if failSubmission { throw NSError(domain: "BGTaskSchedulerErrorDomain", code: 1) }
    submitted.append(request)
  }

  func cancel(_ identifier: String) {
    cancelled.append(identifier)
  }

  /// The system starts the last submitted task.
  @discardableResult
  func start(_ identifier: String? = nil) -> FakeTask {
    let task = FakeTask()
    let id = identifier ?? submitted.last?.identifier ?? ""
    handlers[id]?(task)
    return task
  }
}

@MainActor
final class FakeTask: ContinuedTask {
  let progress = Progress(totalUnitCount: 0)
  var title = ""
  var subtitle = ""
  var expiration: (@MainActor () -> Void)?
  var completed: Bool?

  func update(title: String, subtitle: String) {
    self.title = title
    self.subtitle = subtitle
  }

  func setExpirationHandler(_ handler: @escaping @MainActor () -> Void) {
    expiration = handler
  }

  func complete(success: Bool) {
    completed = success
  }

  func expire() {
    expiration?()
  }
}

@MainActor
final class FakeBackgroundTime: BackgroundTimeProviding {
  var active: [Int: @MainActor () -> Void] = [:]
  var ended: [Int] = []
  private var next = 1

  func begin(name: String, expiration: @escaping @MainActor () -> Void) -> Int? {
    defer { next += 1 }
    active[next] = expiration
    return next
  }

  func end(_ token: Int) {
    active[token] = nil
    ended.append(token)
  }

  func expireAll() {
    for handler in active.values { handler() }
  }
}

/// Fresh, empty defaults for one test.
func scratchDefaults() -> UserDefaults {
  let name = "com.zephleggett.dl-nzb.tests.\(UUID().uuidString)"
  let defaults = UserDefaults(suiteName: name) ?? .standard
  defaults.removePersistentDomain(forName: name)
  return defaults
}

/// A scratch directory removed when the test is done with it.
final class Scratch {
  let url: URL

  init() {
    url = FileManager.default.temporaryDirectory.appending(path: "dl-nzb-ios-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
    try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
  }

  func folder(_ name: String) -> URL {
    let folder = url.appending(path: name, directoryHint: .isDirectory)
    try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    return folder
  }

  /// A small NZB: two RAR volumes and a PAR2 file.
  func nzb(_ title: String, in folder: URL, salt: String = "") throws -> URL {
    var files = ""
    for (index, name) in ["\(title).part01.rar", "\(title).part02.rar", "\(title).par2"].enumerated() {
      files += """
          <file poster="tester" date="1700000000" subject="[\(index + 1)/3] &quot;\(name)&quot; yEnc (1/1)">
            <groups><group>alt.binaries.test</group></groups>
            <segments><segment bytes="768000" number="1">\(salt)\(index)-\(title)@test</segment></segments>
          </file>

        """
    }
    let xml = """
      <?xml version="1.0" encoding="UTF-8"?>
      <nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
      \(files)</nzb>
      """
    let url = folder.appending(path: "\(title).nzb")
    try Data(xml.utf8).write(to: url)
    return url
  }

  deinit {
    try? FileManager.default.removeItem(at: url)
  }
}
