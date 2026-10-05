import Foundation

/// Choosing the name a download goes by, and the folder it lands in.
///
/// An NZB's `<meta type="title">` is sometimes the release name and sometimes
/// an obfuscated file name ("4172R01e3H14n37E65f01G58y82y7191.mkv"), and the
/// file the user opened is sometimes the release name and sometimes "download".
/// The best candidate is the first that is neither. The real engine picks a
/// download's title itself (`NzbInfo.title`); `best` is the simulated
/// engine's stand-in, and `folderName` names every job's folder.
public enum ReleaseName {
  /// The first candidate that reads like a name, or the last resort.
  public static func best(_ candidates: [String?], fallback: String) -> String {
    for candidate in candidates.compactMap({ $0?.trimmingCharacters(in: .whitespacesAndNewlines) }) where !candidate.isEmpty {
      if !looksObfuscated(candidate) && !isGeneric(candidate) { return candidate }
    }
    let trimmed = fallback.trimmingCharacters(in: .whitespacesAndNewlines)
    return trimmed.isEmpty ? "Download" : trimmed
  }

  /// A single unbroken run of letters and digits, long and mixed, or hex: what
  /// posting tools generate to hide a name. Release names have separators.
  public static func looksObfuscated(_ name: String) -> Bool {
    var stem = name
    let ext = (name as NSString).pathExtension
    if !ext.isEmpty, ext.count <= 4 { stem = (name as NSString).deletingPathExtension }
    guard stem.count >= 16 else { return false }
    guard stem.allSatisfy({ $0.isLetter || $0.isNumber }) else { return false }
    let hasDigit = stem.contains(where: \.isNumber)
    let hasLetter = stem.contains(where: \.isLetter)
    let isHex = stem.allSatisfy(\.isHexDigit)
    return isHex || (hasDigit && hasLetter)
  }

  /// Names a browser or an indexer gives a file when it has no better one.
  public static func isGeneric(_ name: String) -> Bool {
    let lower = name.lowercased()
    if ["download", "nzb", "file", "getnzb", "get", "api", "index", "untitled"].contains(lower) { return true }
    return lower.allSatisfy(\.isNumber)
  }

  /// A folder name from a release name: no path separators, no colons (the
  /// Finder shows them as slashes), no leading dots (hidden), and short enough
  /// for any file system to take with a " 2" after it.
  public static func folderName(_ name: String) -> String {
    var cleaned = name.replacingOccurrences(of: "/", with: "-").replacingOccurrences(of: ":", with: "-")
    cleaned = cleaned.components(separatedBy: .controlCharacters).joined()
    cleaned = cleaned.trimmingCharacters(in: .whitespacesAndNewlines)
    while cleaned.hasPrefix(".") { cleaned.removeFirst() }
    if cleaned.utf8.count > 200 {
      var shortened = ""
      for character in cleaned {
        if shortened.utf8.count + character.utf8.count > 200 { break }
        shortened.append(character)
      }
      cleaned = shortened.trimmingCharacters(in: .whitespaces)
    }
    return cleaned.isEmpty ? "Download" : cleaned
  }
}
