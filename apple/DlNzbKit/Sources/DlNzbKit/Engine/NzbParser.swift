import Foundation

/// Reads the parts of an NZB the app shows before the engine has it: the
/// `<head>` metadata and each file's name, size and article count. The real
/// engine parses NZBs itself; this serves the simulated engine, so it can
/// inspect the real files in ~/Downloads, and tests.
public enum NzbParser {
  /// Synchronous file and XML work: call it off the main actor.
  public static func parse(contentsOf url: URL) throws -> NzbInfo {
    let data: Data
    do {
      data = try Data(contentsOf: url)
    } catch {
      throw EngineError(.io, "dl-nzb could not read \(url.lastPathComponent).")
    }
    return try parse(data: data, fileName: url.lastPathComponent)
  }

  public static func parse(data: Data, fileName: String) throws -> NzbInfo {
    let reader = Reader()
    let parser = XMLParser(data: data)
    parser.shouldResolveExternalEntities = false
    parser.delegate = reader
    guard parser.parse(), !reader.files.isEmpty else {
      throw EngineError(.nzb, "\(fileName) is not a valid NZB file.")
    }

    let files = reader.files.map { file in
      NzbFile(name: file.name, bytes: file.bytes, segments: file.segments, kind: NzbFile.kind(forName: file.name))
    }
    let par2Bytes = files.filter { $0.kind == .par2 }.reduce(Int64(0)) { $0 + $1.bytes }
    let totalBytes = files.reduce(Int64(0)) { $0 + $1.bytes }
    let stem = (fileName as NSString).deletingPathExtension
    let title = ReleaseName.best([reader.meta["title"]?.first, metaName(reader.meta["name"]?.first), stem], fallback: stem)
    let kind = ContentKind.dominant(in: files).refined(byReleaseName: title)
    return NzbInfo(
      title: title,
      passwords: reader.meta["password"] ?? [],
      category: reader.meta["category"]?.first,
      totalBytes: totalBytes,
      dataBytes: totalBytes - par2Bytes,
      par2Bytes: par2Bytes,
      files: files,
      contentKind: kind)
  }

  /// A `<meta type="name">` that is a posting subject rather than a name.
  private static func metaName(_ value: String?) -> String? {
    guard let value, !value.contains("\""), !value.contains(" yEnc") else { return nil }
    return value
  }

  /// The file name in a posting subject: the quoted part when there is one
  /// ("[02/17] - "Name.part01.rar" yEnc (1/732)"), else the subject without its
  /// yEnc counter.
  public static func fileName(fromSubject subject: String) -> String {
    let parts = subject.split(separator: "\"", omittingEmptySubsequences: false)
    if parts.count >= 3 {
      let quoted = parts[1].trimmingCharacters(in: .whitespaces)
      if !quoted.isEmpty { return quoted }
    }
    var name = subject
    if let range = name.range(of: #"\s*yEnc\s*(\(\d+/\d+\))?\s*$"#, options: .regularExpression) {
      name.removeSubrange(range)
    }
    if let range = name.range(of: #"\s*\(\d+/\d+\)\s*$"#, options: .regularExpression) {
      name.removeSubrange(range)
    }
    return name.trimmingCharacters(in: .whitespaces)
  }

  /// XMLParser's delegate; lives only for one synchronous parse.
  private final class Reader: NSObject, XMLParserDelegate {
    struct File {
      var name: String
      var bytes: Int64 = 0
      var segments = 0
    }

    var files: [File] = []
    var meta: [String: [String]] = [:]
    private var current: File?
    private var metaType: String?
    private var text = ""

    func parser(
      _ parser: XMLParser, didStartElement elementName: String, namespaceURI: String?, qualifiedName: String?, attributes: [String: String] = [:]
    ) {
      switch elementName {
      case "file":
        current = File(name: NzbParser.fileName(fromSubject: attributes["subject"] ?? ""))
      case "segment":
        current?.segments += 1
        current?.bytes += Int64(attributes["bytes"] ?? "") ?? 0
      case "meta":
        metaType = attributes["type"]?.lowercased()
        text = ""
      default:
        break
      }
    }

    func parser(_ parser: XMLParser, foundCharacters string: String) {
      if metaType != nil { text += string }
    }

    func parser(_ parser: XMLParser, didEndElement elementName: String, namespaceURI: String?, qualifiedName: String?) {
      switch elementName {
      case "file":
        if let current { files.append(current) }
        current = nil
      case "meta":
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if let metaType, !value.isEmpty { meta[metaType, default: []].append(value) }
        metaType = nil
      default:
        break
      }
    }
  }
}
