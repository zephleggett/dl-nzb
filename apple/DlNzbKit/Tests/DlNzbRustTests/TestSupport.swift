import Foundation

/// A folder of its own for one test, removed when the value goes away.
final class ScratchFolder {
  let url: URL

  init() throws {
    url = FileManager.default.temporaryDirectory.appending(path: "DlNzbRustTests-\(UUID().uuidString)", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
  }

  deinit {
    try? FileManager.default.removeItem(at: url)
  }
}

enum Fixture {
  /// A made-up release: a two-article MKV, an NFO and a PAR2 index.
  static var syntheticNZB: URL {
    get throws {
      guard let url = Bundle.module.url(forResource: "synthetic", withExtension: "nzb", subdirectory: "Fixtures") else {
        throw CocoaError(.fileNoSuchFile)
      }
      return url
    }
  }
}

/// A TCP socket bound to a port of the system's choosing on 127.0.0.1, and
/// that port. Nil if it cannot be made; the caller closes `fd`.
func boundLocalSocket() -> (fd: Int32, port: Int)? {
  let fd = socket(AF_INET, SOCK_STREAM, 0)
  guard fd >= 0 else { return nil }
  var address = sockaddr_in()
  address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
  address.sin_family = sa_family_t(AF_INET)
  address.sin_addr.s_addr = inet_addr("127.0.0.1")
  var length = socklen_t(MemoryLayout<sockaddr_in>.size)
  let bound = withUnsafeMutablePointer(to: &address) { pointer in
    pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, length) == 0 && getsockname(fd, $0, &length) == 0 }
  }
  guard bound else {
    close(fd)
    return nil
  }
  return (fd, Int(UInt16(bigEndian: address.sin_port)))
}

/// A TCP port on 127.0.0.1 that nothing listens on: bound, read back, and
/// released.
func unusedLocalPort() -> Int {
  guard let bound = boundLocalSocket() else { return 1 }
  close(bound.fd)
  return bound.port
}
