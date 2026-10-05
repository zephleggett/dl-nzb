import Foundation

/// The Usenet server, without its password (which lives in the Keychain and
/// travels separately so this can be logged, compared and persisted).
public struct ServerSettings: Sendable, Codable, Equatable, Hashable {
  public static let sslPort = 563
  public static let plainPort = 119
  public static let defaultConnectionsMac = 30
  /// Fewer on iPhone and iPad: every connection holds buffers in memory.
  public static let defaultConnectionsMobile = 20
  public static let connectionRange = 1...100
  public static let retryRange = 0...10

  public var host: String
  public var port: Int
  public var useSSL: Bool
  public var verifyCertificate: Bool
  public var username: String
  public var connections: Int
  public var retryAttempts: Int

  public init(
    host: String = "",
    port: Int = ServerSettings.sslPort,
    useSSL: Bool = true,
    verifyCertificate: Bool = true,
    username: String = "",
    connections: Int = ServerSettings.defaultConnections,
    retryAttempts: Int = 2
  ) {
    self.host = host
    self.port = port
    self.useSSL = useSSL
    self.verifyCertificate = verifyCertificate
    self.username = username
    self.connections = connections
    self.retryAttempts = retryAttempts
  }

  public static var defaultConnections: Int {
    #if os(macOS)
      defaultConnectionsMac
    #else
      defaultConnectionsMobile
    #endif
  }

  /// The port SSL implies, 563 or 119.
  public static func defaultPort(useSSL: Bool) -> Int {
    useSSL ? sslPort : plainPort
  }

  /// The host as typed, without spaces or a pasted "nntps://" in front.
  public var normalisedHost: String {
    var host = self.host.trimmingCharacters(in: .whitespacesAndNewlines)
    for scheme in ["nntps://", "nntp://", "news://", "snews://"] where host.lowercased().hasPrefix(scheme) {
      host.removeFirst(scheme.count)
    }
    while host.hasSuffix("/") { host.removeLast() }
    return host
  }
}

/// What happens to a download after its files arrive. Mirrors the CLI's `[post_processing]`.
public struct ProcessingSettings: Sendable, Codable, Equatable, Hashable {
  public var repairWithPar2: Bool
  public var extractArchives: Bool
  public var deleteArchivesAfterExtracting: Bool
  public var deletePar2AfterRepairing: Bool
  public var renameObfuscatedFiles: Bool

  public init(
    repairWithPar2: Bool = true,
    extractArchives: Bool = true,
    deleteArchivesAfterExtracting: Bool = false,
    deletePar2AfterRepairing: Bool = false,
    renameObfuscatedFiles: Bool = true
  ) {
    self.repairWithPar2 = repairWithPar2
    self.extractArchives = extractArchives
    self.deleteArchivesAfterExtracting = deleteArchivesAfterExtracting
    self.deletePar2AfterRepairing = deletePar2AfterRepairing
    self.renameObfuscatedFiles = renameObfuscatedFiles
  }
}

/// The engine settings the Advanced pane holds, beside the server's.
public struct AdvancedSettings: Sendable, Codable, Equatable, Hashable {
  /// The default for new jobs; Download Anyway overrides it for one job.
  public var preflight: Preflight
  /// Fetch every PAR2 volume with the data instead of only when something is missing.
  public var downloadAllRecoveryUpFront: Bool
  /// fsync each finished file. Off: PAR2 verifies integrity anyway.
  public var flushFilesWhenFinished: Bool

  public init(preflight: Preflight = .automatic, downloadAllRecoveryUpFront: Bool = false, flushFilesWhenFinished: Bool = false) {
    self.preflight = preflight
    self.downloadAllRecoveryUpFront = downloadAllRecoveryUpFront
    self.flushFilesWhenFinished = flushFilesWhenFinished
  }
}

/// Everything the engine is configured with. The FFI's flat `EngineConfig` is
/// built from this. Never persisted: it carries the password.
public struct EngineSettings: Sendable, Equatable {
  public var server: ServerSettings
  public var password: String
  public var processing: ProcessingSettings
  public var advanced: AdvancedSettings
  /// Engine-wide; nil is unlimited.
  public var speedLimitBytesPerSecond: Int64?

  public init(
    server: ServerSettings = ServerSettings(),
    password: String = "",
    processing: ProcessingSettings = ProcessingSettings(),
    advanced: AdvancedSettings = AdvancedSettings(),
    speedLimitBytesPerSecond: Int64? = nil
  ) {
    self.server = server
    self.password = password
    self.processing = processing
    self.advanced = advanced
    self.speedLimitBytesPerSecond = speedLimitBytesPerSecond
  }
}

/// Logging or dumping settings never shows the password.
extension EngineSettings: CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable {
  public var description: String {
    let limit = speedLimitBytesPerSecond.map { "\($0) B/s" } ?? "none"
    return
      "EngineSettings(server: \(server), password: <redacted>, processing: \(processing), advanced: \(advanced), speedLimit: \(limit))"
  }

  public var debugDescription: String { description }

  public var customMirror: Mirror {
    Mirror(
      self,
      children: [
        "server": server, "password": "<redacted>", "processing": processing, "advanced": advanced,
        "speedLimitBytesPerSecond": speedLimitBytesPerSecond as Any,
      ])
  }
}

/// Settings read from the dl-nzb CLI's `config.toml`, for the first-launch import.
public struct ImportedSettings: Sendable, Equatable {
  public var server: ServerSettings
  public var password: String
  public var processing: ProcessingSettings
  public var advanced: AdvancedSettings
  /// The CLI's `download.dir`, when it is an absolute path. The app cannot write
  /// there without the user choosing it (sandbox), so it is only a suggestion.
  public var downloadDirectory: URL?
  /// Where the settings came from.
  public var source: URL?

  public init(
    server: ServerSettings,
    password: String,
    processing: ProcessingSettings = ProcessingSettings(),
    advanced: AdvancedSettings = AdvancedSettings(),
    downloadDirectory: URL? = nil,
    source: URL? = nil
  ) {
    self.server = server
    self.password = password
    self.processing = processing
    self.advanced = advanced
    self.downloadDirectory = downloadDirectory
    self.source = source
  }
}

extension ImportedSettings: CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable {
  public var description: String {
    "ImportedSettings(server: \(server), password: <redacted>, source: \(source?.path(percentEncoded: false) ?? "none"))"
  }

  public var debugDescription: String { description }

  public var customMirror: Mirror {
    Mirror(self, children: ["server": server, "password": "<redacted>", "processing": processing, "advanced": advanced, "source": source as Any])
  }
}

/// When finished downloads leave the list. Their files always stay.
public enum RetentionPolicy: String, Sendable, Codable, Equatable, CaseIterable {
  case manually
  case whenAppQuits
  case afterOneDay

  /// On iPhone and iPad the app is rarely quit, and the list is tidied when
  /// it next opens, so the option says so.
  public var title: String {
    switch self {
    case .manually: "Manually"
    #if os(iOS)
      case .whenAppQuits: "When dl-nzb next opens"
    #else
      case .whenAppQuits: "When dl-nzb quits"
    #endif
    case .afterOneDay: "After one day"
    }
  }
}
