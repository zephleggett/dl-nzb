import Foundation

/// Where the queue keeps its list and its copies of the NZBs:
/// Application Support/dl-nzb/queue.json and Application Support/dl-nzb/Queue/.
///
/// The queue copies every NZB it is given, so a file opened from Mail, Safari
/// or a security-scoped Files URL stays readable after the open event is over,
/// across relaunches, and in the sandbox.
public struct QueueStorage: Sendable, Equatable {
  public let directory: URL

  public init(directory: URL) {
    self.directory = directory
  }

  /// Application Support/dl-nzb.
  public static var standard: QueueStorage {
    QueueStorage(directory: AppPaths.applicationSupport)
  }

  /// A fresh folder in the temporary directory, for tests.
  public static func temporary() -> QueueStorage {
    QueueStorage(directory: FileManager.default.temporaryDirectory.appending(path: "dl-nzb-\(UUID().uuidString)", directoryHint: .isDirectory))
  }

  public var queueFile: URL { directory.appending(path: "queue.json") }
  public var nzbDirectory: URL { directory.appending(path: "Queue", directoryHint: .isDirectory) }

  public func nzbURL(for id: UUID) -> URL {
    nzbDirectory.appending(path: "\(id.uuidString).nzb")
  }

  /// What `queue.json` holds. Versioned so a later format can read this one.
  public struct Snapshot: Codable, Sendable, Equatable {
    public var version = 1
    public var items: [DownloadItem]
    /// Pause All was on when the list was saved.
    public var pausedByUser: Bool

    public init(items: [DownloadItem], pausedByUser: Bool = false) {
      self.items = items
      self.pausedByUser = pausedByUser
    }
  }

  /// The saved list, or an empty one when there is none yet. A file that
  /// cannot be read is set aside rather than overwritten, so nothing is lost
  /// to a format mistake.
  public func load() -> Snapshot {
    guard let data = try? Data(contentsOf: queueFile) else { return Snapshot(items: []) }
    do {
      return try Self.decoder.decode(Snapshot.self, from: data)
    } catch {
      Log.persistence.error("queue.json could not be read, setting it aside: \(error.localizedDescription, privacy: .public)")
      let aside = directory.appending(path: "queue-unreadable-\(Int(Date().timeIntervalSince1970)).json")
      try? FileManager.default.moveItem(at: queueFile, to: aside)
      return Snapshot(items: [])
    }
  }

  public func save(_ snapshot: Snapshot) throws {
    try write(try Self.encode(snapshot))
  }

  static func encode(_ snapshot: Snapshot) throws -> Data {
    try encoder.encode(snapshot)
  }

  /// Writes already-encoded list data, atomically.
  func write(_ data: Data) throws {
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    try data.write(to: queueFile, options: .atomic)
  }

  /// Stores an NZB's bytes under the item's id.
  func storeNZB(_ data: Data, for id: UUID) throws -> URL {
    try FileManager.default.createDirectory(at: nzbDirectory, withIntermediateDirectories: true)
    let url = nzbURL(for: id)
    try data.write(to: url, options: .atomic)
    return url
  }

  func removeNZB(for id: UUID) {
    try? FileManager.default.removeItem(at: nzbURL(for: id))
  }

  /// Dates stay in the default encoding, which keeps them exact; ISO 8601
  /// would round them to the second and a restored list would differ.
  private static let encoder: JSONEncoder = {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    return encoder
  }()

  private static let decoder = JSONDecoder()
}
