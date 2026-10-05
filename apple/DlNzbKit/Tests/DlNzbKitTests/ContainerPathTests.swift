import Foundation
import Testing

@testable import DlNzbKit

@Suite("Container paths")
struct ContainerPathTests {
  static let oldHome = "/private/var/mobile/Containers/Data/Application/42898101-F900-4F70-862B-10447B7124EF"
  static let newHome = "/private/var/mobile/Containers/Data/Application/284664F7-CFAA-4D6D-B8D9-0EB95684F83E"

  @Test("A path in an earlier container moves into the current one")
  func rebasesIntoNewContainer() {
    let old = URL(filePath: "\(Self.oldHome)/Documents/Downloads/Big Buck Bunny", directoryHint: .isDirectory)
    let rebased = AppPaths.rebasedIntoCurrentContainer(old, home: Self.newHome)
    #expect(rebased.path(percentEncoded: false) == "\(Self.newHome)/Documents/Downloads/Big Buck Bunny/")
    #expect(rebased.hasDirectoryPath)
  }

  @Test("Paths in the current container, outside any container, or on the Mac stay as they are")
  func leavesOtherPathsAlone() {
    let current = URL(filePath: "\(Self.newHome)/Library/Application Support/dl-nzb/Queue/A.nzb")
    #expect(AppPaths.rebasedIntoCurrentContainer(current, home: Self.newHome) == current)
    let mac = URL(filePath: "/Users/me/Downloads/Release", directoryHint: .isDirectory)
    #expect(AppPaths.rebasedIntoCurrentContainer(mac, home: Self.newHome) == mac)
    #expect(AppPaths.rebasedIntoCurrentContainer(current, home: "/Users/me") == current)
  }

  @Test("A restored item's NZB copy, folder and summary all move")
  func rebasesWholeItem() {
    let folder = URL(filePath: "\(Self.oldHome)/Documents/Downloads/Release", directoryHint: .isDirectory)
    let summary = JobSummary(outcome: .completed, outputDirectory: folder)
    let item = DownloadItem(
      title: "Release", nzbURL: URL(filePath: "\(Self.oldHome)/Library/Application Support/dl-nzb/Queue/A.nzb"), originalFileName: "Release.nzb",
      fingerprint: "f", outputDirectory: folder, state: .finished(summary), summary: summary)
    let rebased = item.rebasedIntoCurrentContainer(home: Self.newHome)
    #expect(rebased.nzbURL.path(percentEncoded: false).hasPrefix(Self.newHome))
    #expect(rebased.outputDirectory.path(percentEncoded: false).hasPrefix(Self.newHome))
    #expect(rebased.summary?.outputDirectory.path(percentEncoded: false).hasPrefix(Self.newHome) == true)
    guard case .finished(let restored) = rebased.state else {
      Issue.record("the state changed")
      return
    }
    #expect(restored.outputDirectory.path(percentEncoded: false).hasPrefix(Self.newHome))
  }
}
