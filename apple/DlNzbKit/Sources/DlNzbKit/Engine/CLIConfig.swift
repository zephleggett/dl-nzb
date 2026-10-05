import Foundation

/// Reads the dl-nzb CLI's `config.toml` in Swift, for the simulated engine
/// and previews (`RustEngine` reads it with the CLI's own parser), and says
/// where the Mac's open panel should look for it.
///
/// Only the CLI's own shape of TOML is understood: `[section]` headers and
/// `key = value` lines with strings, integers and booleans.
public enum CLIConfig {
  /// `~/Library/Application Support/dl-nzb/config.toml` in the user's real home,
  /// not the sandbox container's; where an open panel should start. Nil on iOS.
  public static var defaultURL: URL? {
    #if os(macOS)
      return realHomeDirectory.appending(path: "Library/Application Support/dl-nzb/config.toml")
    #else
      return nil
    #endif
  }

  #if os(macOS)
    /// The home directory from the user database: inside the sandbox,
    /// `NSHomeDirectory()` is the container. Looked up once.
    public static let realHomeDirectory: URL = {
      if let entry = getpwuid(getuid()), let dir = entry.pointee.pw_dir {
        return URL(filePath: String(cString: dir), directoryHint: .isDirectory)
      }
      return URL(filePath: NSHomeDirectory(), directoryHint: .isDirectory)
    }()
  #endif

  /// Reads and converts a config file. Handles a security-scoped URL from an
  /// open panel. Synchronous file work: call it off the main actor.
  public static func read(from url: URL) throws -> ImportedSettings {
    guard let text = url.withSecurityScopedAccess({ try? String(contentsOf: url, encoding: .utf8) }) else {
      throw EngineError(.io, "dl-nzb could not read \(url.lastPathComponent).")
    }
    return try parse(text)
  }

  /// Converts the text of a config file. Throws `EngineError(.config)` when it
  /// names no server, which leaves nothing worth importing.
  public static func parse(_ text: String) throws -> ImportedSettings {
    let table = values(in: text)
    func string(_ key: String) -> String? {
      if case .string(let value) = table[key] { return value }
      return nil
    }
    func int(_ key: String) -> Int? {
      if case .integer(let value) = table[key] { return value }
      return nil
    }
    func bool(_ key: String) -> Bool? {
      if case .boolean(let value) = table[key] { return value }
      return nil
    }

    let host = string("usenet.server")?.trimmingCharacters(in: .whitespaces) ?? ""
    guard !host.isEmpty else {
      throw EngineError(.config, "The dl-nzb settings file does not name a server.")
    }
    let defaults = ServerSettings()
    let useSSL = bool("usenet.ssl") ?? defaults.useSSL
    let server = ServerSettings(
      host: host,
      port: int("usenet.port") ?? ServerSettings.defaultPort(useSSL: useSSL),
      useSSL: useSSL,
      verifyCertificate: bool("usenet.verify_ssl_certs") ?? defaults.verifyCertificate,
      username: string("usenet.username") ?? "",
      connections: (int("usenet.connections") ?? defaults.connections).clamped(to: ServerSettings.connectionRange),
      retryAttempts: (int("usenet.retry_attempts") ?? defaults.retryAttempts).clamped(to: ServerSettings.retryRange))

    let processingDefaults = ProcessingSettings()
    let processing = ProcessingSettings(
      repairWithPar2: bool("post_processing.auto_par2_repair") ?? processingDefaults.repairWithPar2,
      extractArchives: bool("post_processing.auto_extract_rar") ?? processingDefaults.extractArchives,
      deleteArchivesAfterExtracting: bool("post_processing.delete_rar_after_extract") ?? processingDefaults.deleteArchivesAfterExtracting,
      deletePar2AfterRepairing: bool("post_processing.delete_par2_after_repair") ?? processingDefaults.deletePar2AfterRepairing,
      renameObfuscatedFiles: bool("post_processing.deobfuscate_file_names") ?? processingDefaults.renameObfuscatedFiles)

    let advanced = AdvancedSettings(
      preflight: .automatic,
      downloadAllRecoveryUpFront: bool("post_processing.download_all_par2") ?? false,
      flushFilesWhenFinished: bool("tuning.fsync_on_finalize") ?? false)

    return ImportedSettings(
      server: server,
      password: string("usenet.password") ?? "",
      processing: processing,
      advanced: advanced)
  }

  enum Value: Equatable {
    case string(String)
    case integer(Int)
    case boolean(Bool)
  }

  /// Every `section.key` with a value this reader understands.
  static func values(in text: String) -> [String: Value] {
    var section = ""
    var table: [String: Value] = [:]
    for rawLine in text.split(whereSeparator: \.isNewline) {
      let line = rawLine.trimmingCharacters(in: .whitespaces)
      if line.isEmpty || line.hasPrefix("#") { continue }
      if line.hasPrefix("[") {
        section = line.trimmingCharacters(in: CharacterSet(charactersIn: "[] \t"))
        continue
      }
      guard let equals = line.firstIndex(of: "=") else { continue }
      let key = line[..<equals].trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "\"'"))
      let rest = line[line.index(after: equals)...].trimmingCharacters(in: .whitespaces)
      guard let value = value(rest) else { continue }
      table[section.isEmpty ? key : "\(section).\(key)"] = value
    }
    return table
  }

  private static func value(_ text: String) -> Value? {
    if text.hasPrefix("\"") { return basicString(text).map(Value.string) }
    if text.hasPrefix("'") {
      let body = text.dropFirst()
      guard let end = body.firstIndex(of: "'") else { return nil }
      return .string(String(body[..<end]))
    }
    let bare = text.split(separator: "#", maxSplits: 1).first.map { $0.trimmingCharacters(in: .whitespaces) } ?? ""
    if bare == "true" { return .boolean(true) }
    if bare == "false" { return .boolean(false) }
    if let number = Int(bare.replacingOccurrences(of: "_", with: "")) { return .integer(number) }
    return nil
  }

  /// A double-quoted TOML string with its escapes, up to the closing quote.
  private static func basicString(_ text: String) -> String? {
    var result = ""
    var escaping = false
    for character in text.dropFirst() {
      if escaping {
        switch character {
        case "n": result.append("\n")
        case "t": result.append("\t")
        case "\\": result.append("\\")
        case "\"": result.append("\"")
        default: result.append(character)
        }
        escaping = false
      } else if character == "\\" {
        escaping = true
      } else if character == "\"" {
        return result
      } else {
        result.append(character)
      }
    }
    return nil
  }
}
