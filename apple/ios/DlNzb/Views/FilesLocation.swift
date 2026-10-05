import DlNzbKit
import DlNzbUI
import Foundation
import SwiftUI
import UIKit

/// Where downloads are as the Files app shows them, and the links that open
/// Files there. With `UIFileSharingEnabled`, the app's Documents folder is
/// On My iPhone › dl-nzb; the download folder is Documents/Downloads.
@MainActor
enum FilesLocation {
  /// "On My iPhone" or "On My iPad", as Files names the device.
  static var onMyDevice: String {
    "On My \(UIDevice.current.model)"
  }

  /// Documents, symlinks resolved as `displayPath` resolves the folder.
  private static let documentsComponents = URL.documentsDirectory.resolvingSymlinksInPath().standardizedFileURL.pathComponents

  /// "Files › On My iPhone › dl-nzb › Downloads".
  static func displayPath(of folder: URL) -> String {
    let documents = documentsComponents
    let parts = folder.resolvingSymlinksInPath().standardizedFileURL.pathComponents
    let relative = parts.starts(with: documents) ? Array(parts.dropFirst(documents.count)) : [folder.lastPathComponent]
    return (["Files", onMyDevice, "dl-nzb"] + relative).joined(separator: " › ")
  }

  /// A link that opens the Files app at `folder`. Files only opens folders
  /// that exist, so it falls back on the nearest one that does.
  static func filesAppURL(for folder: URL) -> URL? {
    var target = folder
    let documents = URL.documentsDirectory.standardizedFileURL.path(percentEncoded: false)
    while !FileManager.default.fileExists(atPath: target.path(percentEncoded: false)),
      target.standardizedFileURL.path(percentEncoded: false).count > documents.count
    {
      target = target.deletingLastPathComponent()
    }
    var components = URLComponents()
    components.scheme = "shareddocuments"
    components.path = target.path(percentEncoded: false)
    return components.url
  }

  /// The download folder, made if it is not there yet, so Files can open it.
  static func ensureDownloadFolder(_ folder: URL) {
    try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
  }

  /// What Share sends: the largest finished file (the film, the album's
  /// archive), or the folder when there are several of a size or none listed.
  static func shareItem(for item: DownloadItem) -> URL? {
    guard item.isFinished else { return nil }
    // One file holds most of it: that file is the download.
    if let file = item.mainFile(whenShare: { $0 > 0.8 }) { return file }
    let folder = item.summary?.outputDirectory ?? item.outputDirectory
    return FileManager.default.fileExists(atPath: folder.path(percentEncoded: false)) ? folder : nil
  }
}

/// Show in Files, and Share once there is a finished file to send: in the
/// detail's Location section and in a download's menus.
struct FileActionButtons: View {
  let item: DownloadItem
  @Environment(\.openURL) private var openURL

  var body: some View {
    Button("Show in Files", systemImage: "folder") {
      if let url = FilesLocation.filesAppURL(for: item.outputDirectory) { openURL(url) }
    }
    if let share = FilesLocation.shareItem(for: item) {
      ShareLink(item: share) {
        Label("Share", systemImage: "square.and.arrow.up")
      }
    }
  }
}
