import DlNzbKit
import Foundation
import Testing

@testable import DlNzbApp

@Suite("Shortcuts actions")
@MainActor
struct IntentRouterTests {
  /// A live model in its own scratch folder, with no server, so nothing starts.
  final class Harness {
    let folder = FileManager.default.temporaryDirectory.appending(path: "dl-nzb-app-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
    let model: AppModel

    @MainActor
    init() {
      let downloads = folder.appending(path: "Downloads", directoryHint: .isDirectory)
      try? FileManager.default.createDirectory(at: downloads, withIntermediateDirectories: true)
      let settings = SettingsStore.preview(host: "", downloadFolder: downloads)
      model = AppModel(settings: settings, storage: QueueStorage(directory: folder.appending(path: "Support")), simulate: true) { _ in
        SimulatedEngine(configuration: .fast())
      }
    }

    deinit {
      try? FileManager.default.removeItem(at: folder)
    }
  }

  static func nzb(title: String) -> Data {
    Data(
      """
      <?xml version="1.0" encoding="UTF-8"?>
      <nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
        <head><meta type="title">\(title)</meta></head>
        <file poster="tester" date="1700000000" subject="&quot;\(title).mkv&quot; yEnc (1/1)">
          <groups><group>alt.binaries.test</group></groups>
          <segments><segment bytes="768000" number="1">\(title)-1@test</segment></segments>
        </file>
      </nzb>
      """.utf8)
  }

  @Test("Download NZB adds the file to the queue and names it")
  func downloadAdds() async throws {
    let harness = Harness()
    let router = IntentRouter(model: harness.model)
    let reply = try await router.download(data: Self.nzb(title: "Example.Release.2024.1080p.WEB-x264"), fileName: "Example.nzb")
    #expect(reply == "Added “Example.Release.2024.1080p.WEB-x264” to dl-nzb.")
    #expect(harness.model.queue.items.map(\.title) == ["Example.Release.2024.1080p.WEB-x264"])
  }

  @Test("The same NZB again is reported, not added twice")
  func downloadDuplicate() async throws {
    let harness = Harness()
    let router = IntentRouter(model: harness.model)
    let data = Self.nzb(title: "Twice.Over.2024")
    _ = try await router.download(data: data, fileName: "Twice.nzb")
    let reply = try await router.download(data: data, fileName: "Twice.nzb")
    #expect(reply == "“Twice.Over.2024” is in dl-nzb already.")
    #expect(harness.model.queue.items.count == 1)
  }

  @Test("A file that is not an NZB fails with the queue's sentence")
  func downloadRejectsJunk() async {
    let harness = Harness()
    let router = IntentRouter(model: harness.model)
    await #expect(throws: IntentRouter.Failure.self) {
      try await router.download(data: Data("not an nzb".utf8), fileName: "Junk.nzb")
    }
    #expect(harness.model.queue.items.isEmpty)
  }

  @Test("Pause All and Resume All reach the queue")
  func pauseAndResume() {
    let harness = Harness()
    let router = IntentRouter(model: harness.model)
    #expect(router.pauseAll() == "Downloads are paused.")
    #expect(harness.model.queue.isPaused)
    #expect(router.resumeAll() == "Downloads are resuming.")
    #expect(!harness.model.queue.isPaused)
  }
}
