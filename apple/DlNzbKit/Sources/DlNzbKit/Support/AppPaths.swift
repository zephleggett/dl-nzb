import CryptoKit
import Foundation

/// Where dl-nzb keeps its own files. In the sandbox these resolve inside the
/// app's container, which is what we want: the queue is the app's business.
public enum AppPaths {
  /// Application Support/dl-nzb.
  public static var applicationSupport: URL {
    let base =
      FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
      ?? URL(filePath: NSHomeDirectory()).appending(path: "Library/Application Support")
    return base.appending(path: "dl-nzb", directoryHint: .isDirectory)
  }

  /// The default download folder: ~/Downloads on the Mac (the sandbox's
  /// downloads entitlement covers it, and the symlink is resolved so the path
  /// shown is the real one), Documents/Downloads on iPhone and iPad, where the
  /// Files app shows it under On My iPhone › dl-nzb.
  public static var defaultDownloadFolder: URL {
    #if os(macOS)
      let downloads =
        FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
        ?? URL(filePath: NSHomeDirectory()).appending(path: "Downloads")
      return downloads.resolvingSymlinksInPath()
    #else
      let documents =
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
        ?? URL(filePath: NSHomeDirectory()).appending(path: "Documents")
      return documents.appending(path: "Downloads", directoryHint: .isDirectory)
    #endif
  }
}

extension AppPaths {
  /// iOS gives an app's container a new path when the app is updated (the
  /// simulator, when it is reinstalled), so a path saved by an earlier
  /// version can point into a container that is gone. This moves such a path
  /// into the current container, keeping the part after the container's root;
  /// any other path, and every path on the Mac, comes back as it was.
  public static func rebasedIntoCurrentContainer(_ url: URL, home: String = NSHomeDirectory()) -> URL {
    guard url.isFileURL, let old = containerRoot(of: url.path(percentEncoded: false)), let current = containerRoot(of: home + "/"),
      old.root != current.root
    else { return url }
    return URL(filePath: current.root + old.rest, directoryHint: url.hasDirectoryPath ? .isDirectory : .notDirectory)
  }

  /// ".../Containers/Data/Application/<UUID>/" and what follows it.
  private static func containerRoot(of path: String) -> (root: String, rest: String)? {
    let pattern = #"/Containers/Data/Application/[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}/"#
    guard let range = path.range(of: pattern, options: .regularExpression) else { return nil }
    return (String(path[..<range.upperBound]), String(path[range.upperBound...]))
  }
}

extension URL {
  /// The standardised path without a trailing slash ("/" stays itself), so
  /// two spellings of one folder compare equal. Symbolic links are not
  /// followed.
  public var normalisedPath: String {
    var path = standardizedFileURL.path(percentEncoded: false)
    while path.count > 1 && path.hasSuffix("/") { path.removeLast() }
    return path
  }

  /// Whether this lies somewhere within `folder` (not `folder` itself), by
  /// `normalisedPath`.
  public func isInside(_ folder: URL) -> Bool {
    let base = folder.normalisedPath
    let path = normalisedPath
    return path != base && path.hasPrefix(base.hasSuffix("/") ? base : base + "/")
  }

  /// Runs `body` with the security-scoped access this URL carries (an open
  /// panel's, a bookmark's, an open event's) taken up, and gives it back
  /// after. A URL without any, or outside the sandbox, just runs `body`.
  public func withSecurityScopedAccess<T, E: Error>(_ body: () throws(E) -> T) throws(E) -> T {
    let accessing = startAccessingSecurityScopedResource()
    defer { if accessing { stopAccessingSecurityScopedResource() } }
    return try body()
  }
}

/// A stable identity for an NZB's contents, so opening the same file twice is
/// noticed whatever it is called.
public enum NzbFingerprint {
  /// SHA-256 as lowercase hex. NZBs run to hundreds of megabytes: call it off
  /// the main actor.
  public static func of(_ data: Data) -> String {
    var hex: [UInt8] = []
    hex.reserveCapacity(SHA256.byteCount * 2)
    for byte in SHA256.hash(data: data) {
      hex.append(hexDigits[Int(byte >> 4)])
      hex.append(hexDigits[Int(byte & 0x0f)])
    }
    return String(decoding: hex, as: UTF8.self)
  }

  private static let hexDigits = Array("0123456789abcdef".utf8)
}
