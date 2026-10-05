import Foundation

/// What an NZB holds, read without touching the network. Mirrors `dl_nzb::engine::NzbInfo`.
public struct NzbInfo: Sendable, Codable, Equatable {
  /// `<meta type="title">`, or the file's name without its extension.
  public var title: String
  /// From `<meta type="password">`; the engine tries these first.
  public var passwords: [String]
  public var category: String?
  /// Every file, recovery included, as posted (yEnc-encoded sizes).
  public var totalBytes: Int64
  public var dataBytes: Int64
  public var par2Bytes: Int64
  public var files: [NzbFile]
  public var contentKind: ContentKind

  public init(
    title: String,
    passwords: [String] = [],
    category: String? = nil,
    totalBytes: Int64,
    dataBytes: Int64,
    par2Bytes: Int64,
    files: [NzbFile] = [],
    contentKind: ContentKind = .other
  ) {
    self.title = title
    self.passwords = passwords
    self.category = category
    self.totalBytes = totalBytes
    self.dataBytes = dataBytes
    self.par2Bytes = par2Bytes
    self.files = files
    self.contentKind = contentKind
  }

  /// Articles across every file, what a pre-flight scan checks.
  public var articleCount: Int {
    files.reduce(0) { $0 + $1.segments }
  }
}

/// One `<file>` of an NZB.
public struct NzbFile: Sendable, Codable, Equatable, Hashable {
  public enum Kind: String, Sendable, Codable, Equatable {
    case data
    case par2
    case archive
    case other
  }

  public var name: String
  public var bytes: Int64
  public var segments: Int
  public var kind: Kind

  public init(name: String, bytes: Int64, segments: Int, kind: Kind) {
    self.name = name
    self.bytes = bytes
    self.segments = segments
    self.kind = kind
  }

  /// The kind a file name suggests: `.par2`, a RAR, 7z or zip volume, small
  /// sidecars (nfo, sfv, srr, nzb, txt), and everything else as data.
  public static func kind(forName name: String) -> Kind {
    let lower = name.lowercased()
    let ext = (lower as NSString).pathExtension
    if ext == "par2" { return .par2 }
    if ["rar", "7z", "zip"].contains(ext) { return .archive }
    // Old-style RAR volumes (.r00, .r01 …) and split archives (.001, .002 …).
    if ext.count == 3, ext.first == "r", ext.dropFirst().allSatisfy(\.isNumber) { return .archive }
    if ext.count == 3, ext.allSatisfy(\.isNumber) { return .archive }
    if ["nfo", "sfv", "srr", "nzb", "txt", "url", "md5", "sha1"].contains(ext) { return .other }
    return .data
  }
}

/// What a download mostly is, for its icon. Mirrors `dl_nzb::engine::ContentKind`.
public enum ContentKind: String, Sendable, Codable, Equatable, CaseIterable {
  case video
  case audio
  case archive
  case image
  case document
  case software
  case other

  /// The kind a file extension suggests, or nil when it says nothing.
  public static func forExtension(_ ext: String) -> ContentKind? {
    switch ext.lowercased() {
    case "mkv", "mp4", "m4v", "avi", "mov", "wmv", "ts", "m2ts", "webm", "mpg", "mpeg", "vob", "iso":
      .video
    case "flac", "mp3", "m4a", "aac", "ogg", "opus", "wav", "alac", "ape", "m4b", "dsf":
      .audio
    case "rar", "7z", "zip", "tar", "gz":
      .archive
    case "jpg", "jpeg", "png", "gif", "heic", "tif", "tiff", "webp", "raw", "cr2", "nef":
      .image
    case "pdf", "epub", "mobi", "azw3", "cbz", "cbr", "doc", "docx", "txt":
      .document
    case "dmg", "pkg", "app", "exe", "msi", "apk", "ipa":
      .software
    default:
      nil
    }
  }

  /// The kind holding the most bytes. Archives (and RAR volumes) count as
  /// `.archive` here; `refined(byReleaseName:)` looks inside the name.
  public static func dominant(in files: [NzbFile]) -> ContentKind {
    var bytes: [ContentKind: Int64] = [:]
    for file in files where file.kind != .par2 {
      let ext = (file.name as NSString).pathExtension
      let kind: ContentKind = file.kind == .archive ? .archive : (forExtension(ext) ?? .other)
      bytes[kind, default: 0] += file.bytes
    }
    return bytes.max { $0.value < $1.value }?.key ?? .other
  }

  /// Most releases are RAR sets, which say nothing about what is inside, but
  /// their names do: "1080p", "x265" and "S02E04" are video, "FLAC" is audio,
  /// "EPUB" is a book. Leaves any other kind alone.
  public func refined(byReleaseName name: String) -> ContentKind {
    guard self == .archive || self == .other else { return self }
    let tokens = Set(
      name.lowercased()
        .split(whereSeparator: { !$0.isLetter && !$0.isNumber })
        .map(String.init))
    let video: Set<String> = [
      "1080p", "2160p", "720p", "480p", "576p", "4k", "uhd", "x264", "x265", "h264", "h265", "hevc", "avc", "bluray", "bdrip",
      "brrip", "webrip", "hdtv", "dvdrip", "remux", "hdr", "hdr10", "dv", "xvid", "divx", "web",
    ]
    if !tokens.isDisjoint(with: video) { return .video }
    if name.range(of: #"(?i)\bS\d{1,2}E\d{1,3}\b"#, options: .regularExpression) != nil { return .video }
    let audio: Set<String> = ["flac", "mp3", "aac", "320kbps", "v0", "24bit", "16bit", "discography", "album"]
    if !tokens.isDisjoint(with: audio) { return .audio }
    let document: Set<String> = ["epub", "pdf", "mobi", "ebook", "magazine", "comic", "cbr", "cbz"]
    if !tokens.isDisjoint(with: document) { return .document }
    return self
  }
}
