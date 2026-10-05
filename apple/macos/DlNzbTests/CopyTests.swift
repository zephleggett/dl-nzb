import DlNzbKit
import DlNzbUI
import Foundation
import Testing

@testable import DlNzbApp

@Suite("Notifications")
@MainActor
struct NotificationTests {
  @Test("A finished download says so, with the release and its size, and offers Show in Finder")
  func finished() throws {
    let content = try #require(Notifier.content(for: PreviewData.finished))
    #expect(content.title == "Download Finished")
    // Plain text: no invisible break characters in a notification.
    #expect(content.body == "\(PreviewData.sintelTitle) · \(Format.bytes(PreviewData.finished.totalBytes))")
    #expect(content.categoryIdentifier == Notifier.finishedCategory)
    #expect(NotificationText.itemID(from: content.userInfo) == PreviewData.finished.id)
  }

  @Test("A download with problems says so")
  func finishedWithIssues() throws {
    let content = try #require(Notifier.content(for: PreviewData.finishedWithIssues))
    #expect(content.title == "Download Finished with Problems")
  }

  @Test("Failures and questions name the problem instead of the size, with nothing to reveal")
  func problems() throws {
    let failed = try #require(Notifier.content(for: PreviewData.failed))
    #expect(failed.title == "Download Failed")
    #expect(failed.body == "\(PreviewData.cosmosLaundromatTitle) · \(StatusText.line(for: PreviewData.failed))")
    #expect(failed.categoryIdentifier.isEmpty)

    #expect(Notifier.content(for: PreviewData.needsPassword)?.title == "Password Required")
    #expect(Notifier.content(for: PreviewData.needsAttentionUnrepairable)?.title == "Needs Attention")
    #expect(Notifier.content(for: PreviewData.needsSpace)?.title == "Not Enough Space")
  }

  @Test("Nothing to say while a download waits, runs or was stopped")
  func quiet() {
    for item in [PreviewData.queued, PreviewData.downloading, PreviewData.paused, PreviewData.stopped] {
      #expect(Notifier.content(for: item) == nil)
    }
  }
}

@Suite("Menu bar text")
@MainActor
struct MenuBarTextTests {
  @Test("Long release names are cut in the middle, short ones left alone")
  func title() {
    #expect(MenuBarText.title("Short.Name") == "Short.Name")
    let long = "Cosmos.Laundromat.First.Cycle.2015.Directors.Cut.2160p.UHD.BluRay.x265.10bit.HDR-BLENDER"
    let cut = MenuBarText.title(long)
    #expect(cut.count == MenuBarText.titleLimit)
    #expect(cut.hasPrefix("Cosmos.Laundromat"))
    #expect(cut.hasSuffix("-BLENDER"))
    #expect(cut.contains("…"))
  }

  @Test("The rest of the queue reads as the window subtitle does: downloading apart from post-processing")
  func others() {
    let queue = DownloadQueue.preview(items: [PreviewData.downloading, PreviewData.extracting, PreviewData.queued])
    #expect(MenuBarText.others(queue: queue, excluding: PreviewData.downloading.id) == "1 extracting · 1 waiting")
    let alone = DownloadQueue.preview(items: [PreviewData.downloading])
    #expect(MenuBarText.others(queue: alone, excluding: PreviewData.downloading.id) == nil)
  }
}

@Suite("Paths")
struct PathTests {
  @Test("Paths in the home folder read from ~, the real home, not the container")
  func abbreviated() {
    let home = CLIConfig.realHomeDirectory
    #expect(home.appending(path: "Downloads/Film", directoryHint: .isDirectory).abbreviatedPath == "~/Downloads/Film")
    #expect(home.abbreviatedPath == "~")
    #expect(URL(filePath: "/Volumes/Media/Films").abbreviatedPath == "/Volumes/Media/Films")
  }
}
