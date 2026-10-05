import Foundation

/// Sample downloads in every state, for previews, screenshots and the
/// formatting tests, shared by the Mac and iPhone apps. The names are freely
/// licensed works (the Blender open films, a Debian image, a Creative Commons
/// book); the sizes, file counts and article counts are those of real NZBs.
/// Nothing here is used by a running app.
public enum PreviewData {
  // MARK: Releases

  public static let sintelTitle = "Sintel.2010.2160p.UHD.BluRay.x265"
  public static let tearsOfSteelTitle = "Tears.of.Steel.2012.2160p.UHD.BluRay.x265"
  public static let cosmosLaundromatTitle = "Cosmos.Laundromat.2015.1080p.BluRay.x264"
  public static let debianTitle = "debian-12.7.0-amd64-DVD-1"
  public static let bigBuckBunnyTitle = "Big.Buck.Bunny.2008.1080p.BluRay.x264"
  public static let linuxCommandLineTitle = "The_Linux_Command_Line_2nd_Edition"

  public static let downloadFolder = URL(filePath: "/Users/me/Downloads", directoryHint: .isDirectory)

  /// A fixed moment, so previews and snapshots do not change from run to run.
  public static let referenceDate = Date(timeIntervalSinceReferenceDate: 812_800_000)

  /// Sintel: one 8.2 GB MKV and eight PAR2 volumes.
  public static let sintelInfo = NzbInfo(
    title: sintelTitle,
    category: "Movies > UHD",
    totalBytes: 9_562_672_439,
    dataBytes: 8_200_267_250,
    par2Bytes: 1_362_405_189,
    files: [
      NzbFile(name: "\(sintelTitle).mkv", bytes: 8_200_267_250, segments: 9_245, kind: .data),
      NzbFile(name: "\(sintelTitle).mkv.par2", bytes: 40_120, segments: 1, kind: .par2),
      NzbFile(name: "\(sintelTitle).mkv.vol00+01.par2", bytes: 15_536_884, segments: 21, kind: .par2),
      NzbFile(name: "\(sintelTitle).mkv.vol01+02.par2", bytes: 31_002_436, segments: 42, kind: .par2),
      NzbFile(name: "\(sintelTitle).mkv.vol03+04.par2", bytes: 61_945_392, segments: 84, kind: .par2),
      NzbFile(name: "\(sintelTitle).mkv.vol07+08.par2", bytes: 123_868_928, segments: 168, kind: .par2),
      NzbFile(name: "\(sintelTitle).mkv.vol15+16.par2", bytes: 247_706_870, segments: 335, kind: .par2),
      NzbFile(name: "\(sintelTitle).mkv.vol31+32.par2", bytes: 495_278_994, segments: 670, kind: .par2),
      NzbFile(name: "\(sintelTitle).mkv.vol63+25.par2", bytes: 387_025_565, segments: 524, kind: .par2),
    ],
    contentKind: .video)

  public static let tearsOfSteelInfo = rarSet(
    title: tearsOfSteelTitle, totalBytes: 8_601_315_520, par2Bytes: 421_284_584, articles: 11_206, volumes: 78, category: "Movies > UHD")
  public static let cosmosLaundromatInfo = rarSet(title: cosmosLaundromatTitle, totalBytes: 5_111_063_683, par2Bytes: 465_729_566, articles: 6_921, volumes: 9)
  public static let debianInfo = rarSet(
    title: debianTitle, totalBytes: 3_656_770_330, par2Bytes: 336_392_156, articles: 4_767, volumes: 63, contentKind: .software)
  public static let bigBuckBunnyInfo = rarSet(title: bigBuckBunnyTitle, totalBytes: 1_137_217_341, par2Bytes: 104_851_316, articles: 1_489, volumes: 67)
  public static let linuxCommandLineInfo = rarSet(
    title: linuxCommandLineTitle, totalBytes: 38_106_772, par2Bytes: 0, articles: 72, volumes: 36, contentKind: .document, passwords: [])

  /// A RAR set with its PAR2 index and volumes, sizes split evenly.
  public static func rarSet(
    title: String, totalBytes: Int64, par2Bytes: Int64, articles: Int, volumes: Int, category: String? = nil, contentKind: ContentKind = .video,
    passwords: [String] = []
  ) -> NzbInfo {
    let dataBytes = totalBytes - par2Bytes
    let perVolume = dataBytes / Int64(volumes)
    let perArticle = max(articles / (volumes + (par2Bytes > 0 ? 2 : 0)), 1)
    var files = (1...volumes).map { number in
      NzbFile(name: String(format: "%@.part%02d.rar", title, number), bytes: perVolume, segments: perArticle, kind: .archive)
    }
    if par2Bytes > 0 {
      files.append(NzbFile(name: "\(title).par2", bytes: 20_611, segments: 1, kind: .par2))
      files.append(NzbFile(name: "\(title).vol000+100.par2", bytes: par2Bytes - 20_611, segments: perArticle, kind: .par2))
    }
    return NzbInfo(
      title: title, passwords: passwords, category: category, totalBytes: totalBytes, dataBytes: dataBytes, par2Bytes: par2Bytes, files: files,
      contentKind: contentKind)
  }

  // MARK: Items in every state

  public static let queued = item(1, tearsOfSteelInfo, state: .queued)

  public static let connecting = item(2, sintelInfo, state: .running(.connecting), progress: JobProgress(phase: .connecting), visited: [.connecting])

  public static let checking = item(
    3, sintelInfo, state: .running(.checking),
    progress: JobProgress(phase: .checking, filesTotal: 9, fraction: 0.38), visited: [.connecting, .checking])

  /// "3.1 GB of 8.2 GB · 84 MB/s · 1 min left"
  public static let downloading = item(
    4, tearsOfSteelInfo, state: .running(.downloading),
    progress: JobProgress(
      phase: .downloading, bytesDone: 3_100_000_000, bytesTotal: 8_180_030_936, speedBytesPerSecond: 84_400_000, etaSeconds: 60, filesDone: 29,
      filesTotal: 78, fraction: 0.379),
    visited: [.connecting, .checking, .downloading])

  public static let paused = item(
    5, cosmosLaundromatInfo, state: .paused,
    progress: JobProgress(
      phase: .downloading, bytesDone: 1_900_000_000, bytesTotal: 4_645_334_117, filesDone: 3, filesTotal: 9, fraction: 0.409, paused: true),
    visited: [.connecting, .checking, .downloading])

  public static let downloadingRecovery = item(
    6, debianInfo, state: .running(.downloadingRecovery),
    progress: JobProgress(
      phase: .downloadingRecovery, bytesDone: 118_000_000, bytesTotal: 336_392_156, speedBytesPerSecond: 71_000_000, etaSeconds: 3, filesDone: 0,
      filesTotal: 2, articlesFailed: 19, fraction: 0.35),
    visited: [.connecting, .checking, .downloading, .downloadingRecovery])

  public static let verifying = item(
    7, sintelInfo, state: .running(.verifying), progress: JobProgress(phase: .verifying, filesDone: 0, filesTotal: 1, fraction: 0.43),
    visited: [.connecting, .checking, .downloading, .verifying])

  /// "Repairing 12 damaged blocks · 43%"
  public static let repairing = item(
    8, debianInfo, state: .running(.repairing), progress: JobProgress(phase: .repairing, fraction: 0.43, damagedBlocks: 12),
    visited: [.connecting, .checking, .downloading, .downloadingRecovery, .verifying, .repairing])

  /// "Extracting · 2 of 5"
  public static let extracting = item(
    9, cosmosLaundromatInfo, state: .running(.extracting),
    progress: JobProgress(phase: .extracting, filesDone: 1, filesTotal: 5, fraction: 0.31, detail: "2 of 5"),
    visited: [.connecting, .checking, .downloading, .verifying, .extracting])

  public static let renaming = item(
    10, bigBuckBunnyInfo, state: .running(.renaming), progress: JobProgress(phase: .renaming, fraction: 0.5),
    visited: [.connecting, .checking, .downloading, .verifying, .extracting, .renaming])

  /// "8.2 GB · Finished in 3 min · Repaired 12 blocks"
  public static let finishedSummary = JobSummary(
    outcome: .completed, outputDirectory: folder(sintelTitle),
    files: [OutputFile(name: "\(sintelTitle).mkv", bytes: 8_036_261_905)], dataBytes: 8_036_261_905, wireBytes: 8_446_000_000,
    elapsedSeconds: 184, downloadSeconds: 101, articlesTotal: 11_090, articlesFailed: 46,
    par2: Par2Report(ran: true, verifiedOK: false, damagedBlocks: 12, repairedBlocks: 12, repaired: true), filesRenamed: 0)

  public static let finished = item(
    11, sintelInfo, state: .finished(finishedSummary), summary: finishedSummary, finishedAt: referenceDate.addingTimeInterval(-600),
    visited: [.connecting, .checking, .downloading, .downloadingRecovery, .verifying, .repairing])

  public static let finishedWithIssuesSummary = JobSummary(
    outcome: .completedWithIssues, message: "Some files are incomplete because 9% of articles are missing.",
    outputDirectory: folder(bigBuckBunnyTitle), files: [OutputFile(name: "\(bigBuckBunnyTitle).mkv", bytes: 1_001_395_044)],
    dataBytes: 1_001_395_044, wireBytes: 1_170_000_000, elapsedSeconds: 41, downloadSeconds: 17, articlesTotal: 1_489, articlesFailed: 134,
    par2: Par2Report(ran: true, verifiedOK: false, damagedBlocks: 412, repairedBlocks: 0, repaired: false), archivesExtracted: 0, archivesFailed: 1)

  public static let finishedWithIssues = item(
    12, bigBuckBunnyInfo, state: .finished(finishedWithIssuesSummary), summary: finishedWithIssuesSummary,
    finishedAt: referenceDate.addingTimeInterval(-3_600), visited: [.connecting, .downloading, .downloadingRecovery, .verifying, .repairing, .extracting])

  /// "9% of articles missing"
  public static let failedSummary = JobSummary(
    outcome: .failed, message: "9% of articles are missing and there is not enough recovery data.", outputDirectory: folder(cosmosLaundromatTitle),
    dataBytes: 4_645_334_117, wireBytes: 5_111_063_683, elapsedSeconds: 96, downloadSeconds: 61, articlesTotal: 6_921, articlesFailed: 623,
    par2: Par2Report(ran: true, verifiedOK: false, damagedBlocks: 412, repairedBlocks: 0, repaired: false))

  public static let failed = item(
    13, cosmosLaundromatInfo, state: .failed(message: failedSummary.message ?? "", resumable: false), summary: failedSummary,
    visited: [.connecting, .checking, .downloading, .downloadingRecovery, .verifying, .repairing])

  public static let unrepairableAvailability = AvailabilityInfo(
    articlesTotal: 4_767, articlesMissing: 429, missingBytes: 298_834_035, recoveryBytes: 336_392_156, verdict: .unrepairable)

  public static let needsAttentionUnrepairable = item(
    14, debianInfo, state: .needsAttention(.unrepairable(unrepairableAvailability)), availability: unrepairableAvailability,
    visited: [.connecting, .checking])

  public static let needsPassword = item(
    15, linuxCommandLineInfo, state: .needsAttention(.password),
    progress: JobProgress(phase: .extracting, filesTotal: 1),
    visited: [.connecting, .checking, .downloading, .extracting])

  public static let needsSpace = item(
    16, tearsOfSteelInfo, state: .needsAttention(.diskFull("This download needs 3.27 GB more free space.")), visited: [.connecting])

  public static let stopped = item(
    17, bigBuckBunnyInfo, state: .stopped,
    progress: JobProgress(phase: .downloading, bytesDone: 412_000_000, bytesTotal: 1_032_366_025, fraction: 0.4),
    visited: [.connecting, .checking, .downloading])

  /// One of each, in the order a busy list might show them.
  public static let items: [DownloadItem] = [
    downloading, extracting, queued, paused, needsAttentionUnrepairable, needsPassword, failed, finished, finishedWithIssues,
  ]

  /// Every state, for tests and a gallery preview.
  public static let allStates: [DownloadItem] = [
    queued, connecting, checking, downloading, paused, downloadingRecovery, verifying, repairing, extracting, renaming, finished,
    finishedWithIssues, failed, needsAttentionUnrepairable, needsPassword, needsSpace, stopped,
  ]

  // MARK: Helpers

  public static func folder(_ title: String) -> URL {
    downloadFolder.appending(path: title, directoryHint: .isDirectory)
  }

  public static func item(
    _ number: Int, _ info: NzbInfo, state: DownloadItem.State, progress: JobProgress? = nil, summary: JobSummary? = nil,
    availability: AvailabilityInfo? = nil, finishedAt: Date? = nil, visited: [JobPhase] = []
  ) -> DownloadItem {
    let id = UUID(uuidString: String(format: "00000000-0000-0000-0000-%012d", number)) ?? UUID()
    return DownloadItem(
      id: id, title: info.title,
      nzbURL: URL(filePath: "/Users/me/Library/Application Support/dl-nzb/Queue/\(id.uuidString).nzb"),
      originalFileName: "\(info.title).nzb", fingerprint: String(format: "%064d", number), outputDirectory: folder(info.title),
      addedAt: referenceDate.addingTimeInterval(Double(-number * 60)), finishedAt: finishedAt, info: info, state: state, progress: progress,
      summary: summary, availability: availability, visitedPhases: visited)
  }
}
