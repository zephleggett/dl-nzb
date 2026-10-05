import DlNzbKit
import DlNzbUI
import Foundation
import Testing

@testable import DlNzbApp

/// `.onOpenURL` and the file importer, through a real queue and the
/// simulated engine: what gets added, what is asked about, and what is
/// cleaned up.
@MainActor
@Suite("Opening NZBs")
struct OpenRouterTests {
  let scratch = Scratch()
  let queue: DownloadQueue
  let inbox: URL
  let router: OpenRouter

  init() {
    let downloads = scratch.folder("Downloads")
    inbox = scratch.folder("Documents/Inbox")
    let settings = SettingsStore.preview(downloadFolder: downloads)
    let queue = DownloadQueue(
      engine: SimulatedEngine(configuration: .fast(writesFiles: false)), settings: settings,
      storage: QueueStorage(directory: scratch.folder("Support")))
    self.queue = queue
    router = OpenRouter(inboxDirectory: inbox) { urls in await queue.add(urls: urls) }
  }

  @Test("An NZB opened in place is added, kept where it is, and becomes the newest item")
  func opensInPlace() async throws {
    let url = try scratch.nzb("In.Place.Release", in: scratch.folder("iCloud Drive"))
    let results = await router.handle([url])
    guard case .added(let id) = results.first else {
      Issue.record("not added: \(results)")
      return
    }
    #expect(router.lastAdded == id)
    #expect(queue.item(id)?.title == "In.Place.Release")
    #expect(FileManager.default.fileExists(atPath: url.path(percentEncoded: false)))
  }

  @Test("A copy another app left in Documents/Inbox is deleted once read")
  func cleansInbox() async throws {
    let url = try scratch.nzb("Mailed.Release", in: inbox)
    #expect(router.isInInbox(url))
    await router.handle([url])
    #expect(queue.items.count == 1)
    #expect(!FileManager.default.fileExists(atPath: url.path(percentEncoded: false)))
  }

  @Test("Opening the same NZB again asks, and a broken file says why")
  func duplicatesAndFailures() async throws {
    let url = try scratch.nzb("Twice.Release", in: scratch.folder("Files"))
    await router.handle([url])
    await router.handle([url])
    #expect(router.duplicate?.title == "Twice.Release")
    #expect(router.duplicate?.existingItemID == queue.items.first?.id)
    router.dismissDuplicate()
    #expect(router.duplicate == nil)

    let junk = scratch.folder("Files").appending(path: "notes.nzb")
    try Data("not an nzb".utf8).write(to: junk)
    await router.handle([junk])
    #expect(router.failures.count == 1)
    #expect(AlertText.openFailureTitle(router.failures) == "Couldn’t Open “notes.nzb”")
    #expect(queue.items.count == 1)
  }

  @Test("Anything that is not a file is ignored")
  func ignoresOtherURLs() async throws {
    let results = await router.handle([try #require(URL(string: "https://example.com/release.nzb"))])
    #expect(results.isEmpty)
    #expect(queue.items.isEmpty)
    #expect(router.failures.isEmpty)
  }
}
