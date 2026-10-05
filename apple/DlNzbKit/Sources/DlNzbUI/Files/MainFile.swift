import DlNzbKit
import Foundation

extension DownloadItem {
  /// The largest file the latest run left, where the job put it; nil when
  /// it listed none.
  public var largestFile: URL? {
    largestOutput?.url
  }

  /// The file that is the download (a film, an ISO, an album's archive):
  /// the largest file the latest run left, when `holdsEnough` accepts its
  /// share of the bytes (0...1, the rest being sidecars such as nfo and
  /// sfv) and the file is there. Nil for a set of files, or none.
  public func mainFile(whenShare holdsEnough: (Double) -> Bool) -> URL? {
    guard let summary, let largest = largestOutput else { return nil }
    let total = summary.files.reduce(Int64(0)) { $0 + $1.bytes }
    guard total > 0, holdsEnough(Double(largest.file.bytes) / Double(total)) else { return nil }
    return FileManager.default.fileExists(atPath: largest.url.path(percentEncoded: false)) ? largest.url : nil
  }

  private var largestOutput: (file: OutputFile, url: URL)? {
    guard let summary, let file = summary.files.max(by: { $0.bytes < $1.bytes }) else { return nil }
    return (file, summary.outputDirectory.appending(path: file.name, directoryHint: .notDirectory))
  }
}
