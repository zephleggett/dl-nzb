import Foundation

/// What a successful Test Connection learnt. Mirrors `dl_nzb::engine::ServerCheck`.
public struct ServerCheck: Sendable, Codable, Equatable {
  /// The server's welcome line, "200 news.example.com NNRP Service Ready".
  public var greeting: String
  public var tls: Bool
  public var latencyMilliseconds: Int

  public init(greeting: String, tls: Bool, latencyMilliseconds: Int) {
    self.greeting = greeting
    self.tls = tls
    self.latencyMilliseconds = latencyMilliseconds
  }
}
