import Foundation

/// One download in the list: the NZB it came from, where it goes, and what
/// state it is in. A value, so views switch on `state` and tests compare whole
/// items; `DownloadQueue` owns the live ones.
public struct DownloadItem: Identifiable, Sendable, Codable, Equatable {
  /// Where an item is in its life. Persisted as is, except that a running item
  /// is restored as `.queued` and continues from its folder.
  public enum State: Sendable, Codable, Equatable {
    /// Waiting for its turn, or for Start when downloads do not start automatically.
    case queued
    case running(JobPhase)
    case paused
    /// Stopped short of finishing on something the user can fix.
    case needsAttention(Attention)
    case finished(JobSummary)
    case failed(message: String, resumable: Bool)
    /// Stopped by the user. Retry continues it while its data is there.
    case stopped
  }

  /// What the user is being asked about.
  public enum Attention: Sendable, Codable, Equatable {
    /// The pre-flight scan found more missing than PAR2 can rebuild. Offers
    /// Download Anyway and Remove.
    case unrepairable(AvailabilityInfo)
    /// Downloaded, but an archive needs a password. Offers a password field.
    case password
    /// Not enough space; the sentence gives the shortfall. Offers Retry.
    case diskFull(String)
  }

  public let id: UUID
  public var title: String
  /// The queue's own copy, Application Support/dl-nzb/Queue/<id>.nzb.
  public var nzbURL: URL
  /// The file name the user opened, for the inspector.
  public var originalFileName: String
  /// SHA-256 of the NZB, to notice the same file opened twice.
  public var fingerprint: String
  /// The exact job folder; unique among the list and on disk when added.
  public var outputDirectory: URL
  public var addedAt: Date
  public var finishedAt: Date?
  public var info: NzbInfo?
  public var state: State
  /// The latest progress of the latest run.
  public var progress: JobProgress?
  /// The summary of the latest run, whatever its outcome.
  public var summary: JobSummary?
  public var availability: AvailabilityInfo?
  /// Phases this item has been through, in order, for the phase checklist.
  public var visitedPhases: [JobPhase]
  public var warnings: [String]
  /// Passwords the user typed, tried after the NZB's own.
  public var passwords: [String]
  /// Started by the user (Start, Resume, Retry) rather than by the queue's
  /// own turn-taking; such an item runs even when downloads do not start
  /// automatically.
  public var startRequested: Bool
  /// Set by Download Anyway: skip the scan and download whatever is there.
  public var downloadAnyway: Bool
  /// Time spent in earlier runs that did not finish the job (stopped by
  /// quitting, Stop, a server problem), added to the run that finishes it.
  /// Optional so lists saved before it existed still load.
  public var earlierRunSeconds: Double?
  /// 2, 3 … for the second and later copies of a release in the list (Download
  /// Again, or two NZBs with the same name), whose folders are "Name 2",
  /// "Name 3". Nil for the first. Optional so older lists still load.
  public var copyNumber: Int?

  public init(
    id: UUID = UUID(),
    title: String,
    nzbURL: URL,
    originalFileName: String,
    fingerprint: String,
    outputDirectory: URL,
    addedAt: Date = Date(),
    finishedAt: Date? = nil,
    info: NzbInfo? = nil,
    state: State = .queued,
    progress: JobProgress? = nil,
    summary: JobSummary? = nil,
    availability: AvailabilityInfo? = nil,
    visitedPhases: [JobPhase] = [],
    warnings: [String] = [],
    passwords: [String] = [],
    startRequested: Bool = false,
    downloadAnyway: Bool = false,
    copyNumber: Int? = nil
  ) {
    self.id = id
    self.title = title
    self.nzbURL = nzbURL
    self.originalFileName = originalFileName
    self.fingerprint = fingerprint
    self.outputDirectory = outputDirectory
    self.addedAt = addedAt
    self.finishedAt = finishedAt
    self.info = info
    self.state = state
    self.progress = progress
    self.summary = summary
    self.availability = availability
    self.visitedPhases = visitedPhases
    self.warnings = warnings
    self.passwords = passwords
    self.startRequested = startRequested
    self.downloadAnyway = downloadAnyway
    self.copyNumber = copyNumber
  }

  // MARK: Names

  /// The name a row shows: the release name, with "(2)" after a second copy
  /// so two rows of one release can be told apart. `title` stays the release
  /// name, for the engine, Copy Name and the folder.
  public var displayTitle: String {
    guard let copyNumber, copyNumber > 1 else { return title }
    return "\(title) (\(copyNumber))"
  }

  // MARK: What the state allows

  /// The phase when running.
  public var phase: JobPhase? {
    if case .running(let phase) = state { return phase }
    return nil
  }

  public var isRunning: Bool { phase != nil }

  /// Running in a phase that holds server connections.
  public var usesNetwork: Bool { phase?.usesNetwork ?? false }

  public var isQueued: Bool { state == .queued }
  public var isPaused: Bool { state == .paused }

  /// Finished with files to open (not failed, not stopped).
  public var isFinished: Bool {
    if case .finished(let summary) = state { return summary.outcome.isSuccess }
    return false
  }

  public var needsAttention: Bool {
    if case .needsAttention = state { return true }
    return false
  }

  /// Asked for a password again: the user has typed one and it did not open
  /// the archive.
  public var passwordRejected: Bool {
    if case .needsAttention(.password) = state { return !passwords.isEmpty }
    return false
  }

  /// The latest progress when it reports on the phase the item is running
  /// in; nil while not running, or before the phase has reported.
  public var phaseProgress: JobProgress? {
    guard let phase, progress?.phase == phase else { return nil }
    return progress
  }

  /// Not finished, failed or stopped: what quitting would interrupt.
  public var isUnfinished: Bool {
    switch state {
    case .queued, .running, .paused: true
    case .needsAttention, .finished, .failed, .stopped: false
    }
  }

  /// Pause is offered while waiting or before post-processing starts; the
  /// engine cannot pause verifying, repairing or extracting.
  public var canPause: Bool {
    switch state {
    case .queued: true
    case .running(let phase): phase.usesNetwork
    default: false
    }
  }

  public var canResume: Bool { state == .paused }

  /// Stop is offered for anything not yet done.
  public var canStop: Bool {
    switch state {
    case .queued, .running, .paused: true
    default: false
    }
  }

  public var canRetry: Bool {
    switch state {
    case .failed, .stopped: true
    case .needsAttention(.diskFull): true
    default: false
    }
  }

  /// Something is in the job folder that Stop could keep or delete. The
  /// engine writes its resume data as downloading begins, so a job that has
  /// only connected or checked has nothing to ask about; one that downloaded
  /// before (and is connecting again) has. No progress means none yet, or
  /// deleted.
  public var hasData: Bool {
    guard let progress else { return false }
    return progress.bytesDone > 0 || visitedPhases.contains { $0 != .connecting && $0 != .checking }
  }

  /// Remove from List should ask first: the item is running (removing it
  /// stops it), or something it downloaded is in its folder and has not
  /// become a finished download, so the user may want it deleted rather than
  /// left behind. A finished download's files always stay, so it never asks.
  public var needsRemovalConfirmation: Bool {
    switch state {
    case .finished: false
    case .running: true
    case .queued, .paused, .needsAttention, .failed, .stopped: hasData
    }
  }

  /// The queue's turn-taking would start this item: it is waiting, and
  /// downloads start `automatically` or the user asked for it.
  public func mayStart(automatically: Bool) -> Bool {
    isQueued && (automatically || startRequested)
  }

  // MARK: Paths

  /// The item with every saved path moved into the app's current container
  /// (see `AppPaths.rebasedIntoCurrentContainer`), for a list restored after
  /// an update on iPhone or iPad. Unchanged on the Mac.
  public func rebasedIntoCurrentContainer(home: String = NSHomeDirectory()) -> DownloadItem {
    var item = self
    item.nzbURL = AppPaths.rebasedIntoCurrentContainer(nzbURL, home: home)
    item.outputDirectory = AppPaths.rebasedIntoCurrentContainer(outputDirectory, home: home)
    if let folder = summary?.outputDirectory {
      item.summary?.outputDirectory = AppPaths.rebasedIntoCurrentContainer(folder, home: home)
    }
    if case .finished(var summary) = item.state {
      summary.outputDirectory = AppPaths.rebasedIntoCurrentContainer(summary.outputDirectory, home: home)
      item.state = .finished(summary)
    }
    return item
  }

  // MARK: Sizes

  /// The release's size as every line shows it: its data files, as the NZB
  /// states them. Recovery files are left out, as they download only when
  /// something is missing, so "Waiting · 5.8 GB", "3.1 GB of 5.8 GB" and the
  /// finished "5.8 GB" agree (the engine counts a download in the same unit).
  /// An NZB that splits out nothing falls back to its whole size.
  public var totalBytes: Int64 {
    guard let info else { return 0 }
    return info.dataBytes > 0 ? info.dataBytes : info.totalBytes
  }

  /// The size to show: the release's, or, finished from an NZB that gave
  /// none, what the job wrote.
  public var displayBytes: Int64 {
    if totalBytes <= 0, case .finished(let summary) = state { return summary.dataBytes }
    return totalBytes
  }

  /// How much of the download (not of post-processing) is done, 0...1. A
  /// job past its download phases counts as fully downloaded.
  public var downloadFraction: Double {
    switch state {
    case .running(let phase):
      if phase.isTransfer { return progress?.displayFraction ?? 0 }
      return phase.usesNetwork ? 0 : 1
    case .finished, .needsAttention(.password):
      return 1
    case .queued, .paused, .needsAttention, .failed, .stopped:
      // Where the transfer got to before it stopped.
      guard let progress, progress.phase.isTransfer else { return 0 }
      return progress.displayFraction
    }
  }

  /// What the finished job left in its folder, or nil before then (or when
  /// it reported nothing).
  public var outputFiles: [OutputFile]? {
    guard isFinished, let files = summary?.files, !files.isEmpty else { return nil }
    return files
  }

  /// The files to list: what the job left once it is done, until then what
  /// the NZB holds. Recovery files are left out, and the biggest come first
  /// (the film before its subtitles), then by name as Finder sorts them.
  public var listedFiles: [OutputFile] {
    let all = outputFiles ?? (info?.files ?? []).map { OutputFile(name: $0.name, bytes: $0.bytes) }
    return all.filter { NzbFile.kind(forName: $0.name) != .par2 }.sorted {
      $0.bytes != $1.bytes ? $0.bytes > $1.bytes : $0.name.localizedStandardCompare($1.name) == .orderedAscending
    }
  }

  /// What the content looks like, for its icon: the engine's kind, sharpened
  /// by the release name for archive sets.
  public var contentKind: ContentKind {
    (info?.contentKind ?? .other).refined(byReleaseName: title)
  }
}
