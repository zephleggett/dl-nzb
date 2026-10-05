import Foundation
import Security
import Synchronization

/// The account a server password belongs to: what the Keychain item is keyed on.
public struct NNTPAccount: Sendable, Codable, Hashable {
  public var server: String
  public var port: Int
  public var username: String

  public init(server: String, port: Int, username: String) {
    self.server = server
    self.port = port
    self.username = username
  }
}

/// Where the server password lives. The Keychain in the apps; memory in
/// previews and tests, so they never touch the user's keychain.
public protocol PasswordStore: Sendable {
  func password(for account: NNTPAccount) -> String?
  func setPassword(_ password: String, for account: NNTPAccount) throws
  func deletePassword(for account: NNTPAccount)
}

/// The server password as an internet password item (NNTPS, server, port,
/// account), the shape Keychain Access shows with the server's name and that
/// password managers recognise.
public struct Keychain: PasswordStore {
  public init() {}

  private func query(for account: NNTPAccount) -> [String: Any] {
    [
      kSecClass as String: kSecClassInternetPassword,
      kSecAttrServer as String: account.server,
      kSecAttrPort as String: account.port,
      kSecAttrAccount as String: account.username,
      kSecAttrProtocol as String: kSecAttrProtocolNNTPS,
    ]
  }

  public func password(for account: NNTPAccount) -> String? {
    guard !account.server.isEmpty else { return nil }
    var query = query(for: account)
    query[kSecReturnData as String] = true
    query[kSecMatchLimit as String] = kSecMatchLimitOne
    var result: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &result)
    guard status == errSecSuccess, let data = result as? Data else {
      if status != errSecItemNotFound {
        Log.settings.error("reading the server password failed: \(status, privacy: .public)")
      }
      return nil
    }
    return String(data: data, encoding: .utf8)
  }

  public func setPassword(_ password: String, for account: NNTPAccount) throws {
    guard !account.server.isEmpty else { return }
    let data = Data(password.utf8)
    let query = query(for: account)
    let update: [String: Any] = [kSecValueData as String: data]
    var status = SecItemUpdate(query as CFDictionary, update as CFDictionary)
    if status == errSecItemNotFound {
      var item = query
      item[kSecValueData as String] = data
      item[kSecAttrLabel as String] = "dl-nzb (\(account.server))"
      #if os(iOS)
        // The queue keeps running in a continued-processing task with the
        // screen locked, and it needs the password to reconnect.
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
      #endif
      status = SecItemAdd(item as CFDictionary, nil)
    }
    guard status == errSecSuccess else {
      Log.settings.error("saving the server password failed: \(status, privacy: .public)")
      throw KeychainError(status: status)
    }
  }

  public func deletePassword(for account: NNTPAccount) {
    guard !account.server.isEmpty else { return }
    let status = SecItemDelete(query(for: account) as CFDictionary)
    if status != errSecSuccess && status != errSecItemNotFound {
      Log.settings.error("removing the server password failed: \(status, privacy: .public)")
    }
  }
}

public struct KeychainError: Error, LocalizedError, Equatable {
  public let status: OSStatus

  public var errorDescription: String? {
    "The password could not be saved in the keychain (\(status))."
  }
}

/// Passwords in memory, for previews and tests.
public final class InMemoryPasswordStore: PasswordStore {
  private let passwords: Mutex<[NNTPAccount: String]>

  public init(_ passwords: [NNTPAccount: String] = [:]) {
    self.passwords = Mutex(passwords)
  }

  public func password(for account: NNTPAccount) -> String? {
    passwords.withLock { $0[account] }
  }

  public func setPassword(_ password: String, for account: NNTPAccount) throws {
    passwords.withLock { $0[account] = password }
  }

  public func deletePassword(for account: NNTPAccount) {
    _ = passwords.withLock { $0.removeValue(forKey: account) }
  }

  /// Every stored account, for tests.
  public var accounts: Set<NNTPAccount> {
    passwords.withLock { Set($0.keys) }
  }
}
