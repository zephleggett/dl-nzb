import Foundation
import Testing

@testable import DlNzbKit

/// Writes small NZBs for the tests: a RAR set and its PAR2 files, sized so the
/// simulated engine has something to download.
enum TestNZB {
  /// - Parameters:
  ///   - title: The `<meta type="title">`, which also names the folder and so
  ///     picks the simulated scenario.
  ///   - dataBytes: Split over `volumes` RAR volumes.
  ///   - salt: Changes the bytes without changing the title, for two NZBs
  ///     of the same release.
  ///   - titled: False leaves the `<meta type="title">` out, so the NZB is
  ///     named after its file.
  static func xml(
    title: String, dataBytes: Int64 = 100_000_000, par2Bytes: Int64 = 8_000_000, volumes: Int = 4, password: String? = nil, salt: String = "",
    titled: Bool = true
  ) -> String {
    let segmentBytes: Int64 = 768_000
    var files = ""
    func file(_ name: String, _ bytes: Int64) {
      let count = max(Int((bytes + segmentBytes - 1) / segmentBytes), 1)
      var segments = ""
      var remaining = bytes
      for number in 1...count {
        let size = min(remaining, segmentBytes)
        remaining -= size
        segments += "      <segment bytes=\"\(size)\" number=\"\(number)\">\(salt)\(name.hashValue & 0xffff)-\(number)@test</segment>\n"
      }
      files += """
          <file poster="tester" date="1700000000" subject="[1/\(volumes)] &quot;\(name)&quot; yEnc (1/\(count))">
            <groups><group>alt.binaries.test</group></groups>
            <segments>
        \(segments)    </segments>
          </file>

        """
    }
    for volume in 1...volumes {
      file(String(format: "%@.part%02d.rar", title, volume), dataBytes / Int64(volumes))
    }
    if par2Bytes > 0 {
      file("\(title).par2", 20_000)
      file("\(title).vol00+10.par2", par2Bytes - 20_000)
    }
    let titleMeta = titled ? "    <meta type=\"title\">\(title)</meta>\n" : ""
    let passwordMeta = password.map { "    <meta type=\"password\">\($0)</meta>\n" } ?? ""
    return """
      <?xml version="1.0" encoding="UTF-8"?>
      <!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
      <nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
        <head>
      \(titleMeta)\(passwordMeta)  </head>
      \(files)</nzb>
      """
  }

  /// Writes an NZB named after its title into `directory`.
  @discardableResult
  static func write(
    title: String, in directory: URL, fileName: String? = nil, dataBytes: Int64 = 100_000_000, par2Bytes: Int64 = 8_000_000, volumes: Int = 4,
    password: String? = nil, salt: String = "", titled: Bool = true
  ) throws -> URL {
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    let url = directory.appending(path: fileName ?? "\(title).nzb")
    let text = xml(title: title, dataBytes: dataBytes, par2Bytes: par2Bytes, volumes: volumes, password: password, salt: salt, titled: titled)
    try Data(text.utf8).write(to: url)
    return url
  }
}

/// A scratch directory removed when the test is done with it.
final class Scratch {
  let url: URL

  init() {
    url = FileManager.default.temporaryDirectory.appending(path: "dl-nzb-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
    try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
  }

  func folder(_ name: String) -> URL {
    let folder = url.appending(path: name, directoryHint: .isDirectory)
    try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    return folder
  }

  deinit {
    try? FileManager.default.removeItem(at: url)
  }
}

/// Polls until `condition` holds or `timeout` passes. True when it held.
@MainActor
func eventually(timeout: Duration = .seconds(10), _ condition: @MainActor () -> Bool) async -> Bool {
  let deadline = ContinuousClock.now + timeout
  while ContinuousClock.now < deadline {
    if condition() { return true }
    try? await Task.sleep(for: .milliseconds(2))
  }
  return condition()
}

/// Everything a queue test needs, in its own scratch space.
@MainActor
final class QueueHarness {
  let scratch = Scratch()
  let engine: SimulatedEngine
  let settings: SettingsStore
  let storage: QueueStorage
  let queue: DownloadQueue
  let downloads: URL
  let inbox: URL

  /// - Parameter timeScale: 20,000 runs a 100 MB job in a blink; 250 gives
  ///   a test time to act while a job is downloading.
  init(timeScale: Double = 20_000, startAutomatically: Bool = true, storage: QueueStorage? = nil, engine: SimulatedEngine? = nil) {
    downloads = scratch.folder("Downloads")
    inbox = scratch.folder("Inbox")
    self.engine = engine ?? SimulatedEngine(configuration: .fast(timeScale: timeScale))
    settings = .preview(downloadFolder: downloads)
    settings.startAutomatically = startAutomatically
    self.storage = storage ?? QueueStorage(directory: scratch.folder("Support"))
    queue = DownloadQueue(engine: self.engine, settings: settings, storage: self.storage)
  }

  /// Gives the engine the settings and lets the queue run, as `AppModel.launch` does.
  func launch() async throws {
    try await engine.apply(settings.engineSettings)
    queue.restore()
    queue.activate()
  }

  /// Adds an NZB with this title and returns its item's id.
  func add(_ title: String, dataBytes: Int64 = 100_000_000, par2Bytes: Int64 = 8_000_000, password: String? = nil) async throws -> UUID {
    let url = try TestNZB.write(title: title, in: inbox, dataBytes: dataBytes, par2Bytes: par2Bytes, password: password)
    let result = await queue.add(url)
    guard case .added(let id) = result else {
      Issue.record("adding \(title) gave \(result)")
      throw CancellationError()
    }
    return id
  }

  func state(_ id: UUID) -> DownloadItem.State? {
    queue.item(id)?.state
  }

  func isFinished(_ id: UUID) -> Bool {
    queue.item(id)?.isFinished ?? false
  }

  /// Holds every download at a crawl, so a test can act while one runs.
  func throttle() async {
    await engine.setSpeedLimit(bytesPerSecond: 50_000)
  }

  func unthrottle() async {
    await engine.setSpeedLimit(bytesPerSecond: nil)
  }

  func waitUntilDownloading(_ id: UUID) async -> Bool {
    await eventually { [queue] in
      queue.item(id)?.phase == .downloading && (queue.item(id)?.progress?.bytesDone ?? 0) > 0
    }
  }
}

extension DownloadItem.State {
  var isNeedsPassword: Bool {
    if case .needsAttention(.password) = self { return true }
    return false
  }

  var isUnrepairable: Bool {
    if case .needsAttention(.unrepairable) = self { return true }
    return false
  }

  var finishedOutcome: Outcome? {
    if case .finished(let summary) = self { return summary.outcome }
    return nil
  }
}
