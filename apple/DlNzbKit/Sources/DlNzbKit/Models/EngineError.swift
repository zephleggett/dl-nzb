import Foundation

/// An error from the engine, with the kind the queue acts on and a sentence for
/// the user. Mirrors the FFI's `EngineError` (`dl_nzb::ErrorKind` plus message).
public struct EngineError: Error, Sendable, Codable, Equatable, LocalizedError {
  /// Mirrors `dl_nzb::ErrorKind`.
  public enum Kind: String, Sendable, Codable, Equatable, CaseIterable {
    case config
    case auth
    case dns
    case connect
    case tls
    case timeout
    case `protocol`
    case nzb
    case io
    case diskFull

    /// Problems with the server or the way to it rather than with a job. The
    /// queue pauses on these instead of failing every job in turn.
    public var isServerProblem: Bool {
      switch self {
      case .auth, .dns, .connect, .tls, .timeout: true
      case .config, .protocol, .nzb, .io, .diskFull: false
      }
    }

    /// What to say when the engine gave no message of its own.
    public var defaultMessage: String {
      switch self {
      case .config: "The settings are incomplete. Check the server settings."
      case .auth: "The server did not accept the username or password."
      case .dns: "The server’s name could not be found. Check the host name."
      case .connect: "The server could not be reached. Check your connection and the server’s port."
      case .tls: "A secure connection to the server could not be made."
      case .timeout: "The server took too long to answer."
      case .protocol: "The server sent a reply dl-nzb did not understand."
      case .nzb: "This file is not a valid NZB."
      case .io: "dl-nzb could not read or write a file in the download folder."
      case .diskFull: "There is not enough free space for this download."
      }
    }
  }

  public var kind: Kind
  public var message: String

  public init(_ kind: Kind, _ message: String? = nil) {
    self.kind = kind
    let trimmed = message?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    self.message = trimmed.isEmpty ? kind.defaultMessage : trimmed
  }

  public var errorDescription: String? { message }
}
