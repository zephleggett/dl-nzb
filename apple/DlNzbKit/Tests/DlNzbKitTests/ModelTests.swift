import Foundation
import Testing

@testable import DlNzbKit

/// Reading NZBs and the CLI's config, and choosing names.
@Suite("Models")
struct ModelTests {
  // MARK: NZB parsing

  @Test("An NZB's metadata, files, sizes and article counts are read")
  func parsesAnNZB() throws {
    let text = TestNZB.xml(title: "Some.Show.S01E02.1080p.WEB-DL", dataBytes: 40_000_000, par2Bytes: 4_000_000, volumes: 4, password: "hunter2")
    let info = try NzbParser.parse(data: Data(text.utf8), fileName: "whatever.nzb")
    #expect(info.title == "Some.Show.S01E02.1080p.WEB-DL")
    #expect(info.passwords == ["hunter2"])
    #expect(info.files.count == 6)
    #expect(info.files.filter { $0.kind == .archive }.count == 4)
    #expect(info.files.filter { $0.kind == .par2 }.count == 2)
    #expect(info.dataBytes == 40_000_000)
    #expect(info.par2Bytes == 4_000_000)
    #expect(info.totalBytes == 44_000_000)
    #expect(info.articleCount == info.files.reduce(0) { $0 + $1.segments })
    // A RAR set, but the name says what is inside.
    #expect(info.contentKind == .video)
  }

  @Test("Something that is not an NZB is an nzb error")
  func rejectsGarbage() {
    #expect(throws: EngineError.self) { try NzbParser.parse(data: Data("hello".utf8), fileName: "notes.nzb") }
    #expect(throws: EngineError.self) { try NzbParser.parse(data: Data("<nzb></nzb>".utf8), fileName: "empty.nzb") }
  }

  @Test("File names come from the quoted part of a subject")
  func subjects() {
    #expect(NzbParser.fileName(fromSubject: "[02/17] - \"Cosmos.Laundromat.part01.rar\" yEnc (1/732)") == "Cosmos.Laundromat.part01.rar")
    #expect(NzbParser.fileName(fromSubject: "Some.File.mkv yEnc (1/20)") == "Some.File.mkv")
    #expect(NzbParser.fileName(fromSubject: "plain name (3/9)") == "plain name")
  }

  @Test("An obfuscated meta title gives way to the meta name, then to the file name")
  func titles() throws {
    let obfuscated = """
      <nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><head>
      <meta type="title">4172R01e3H14n37E65f01G58y82y7191.mkv</meta>
      <meta type="name">Tears.of.Steel.2012.2160p</meta>
      <meta type="category">Movies &gt; UHD</meta></head>
      <file subject="&quot;a.rar&quot; yEnc (1/1)"><segments><segment bytes="10" number="1">a@b</segment></segments></file></nzb>
      """
    let info = try NzbParser.parse(data: Data(obfuscated.utf8), fileName: "download.nzb")
    #expect(info.title == "Tears.of.Steel.2012.2160p")
    #expect(info.category == "Movies > UHD")

    let subjectName = """
      <nzb><head><meta type="name">[04/36] &quot;Mag.part04.rar&quot; yEnc</meta></head>
      <file subject="&quot;Mag.part01.rar&quot; yEnc (1/1)"><segments><segment bytes="10" number="1">a@b</segment></segments></file></nzb>
      """
    #expect(try NzbParser.parse(data: Data(subjectName.utf8), fileName: "The_Linux_Command_Line.nzb").title == "The_Linux_Command_Line")
  }

  @Test(
    "The real NZBs in Downloads can be inspected",
    .enabled(if: !ModelTests.realNZBs.isEmpty, "no NZBs in ~/Downloads"))
  func realFiles() async throws {
    let engine = SimulatedEngine(configuration: .fast())
    for url in Self.realNZBs {
      let info = try await engine.inspect(url)
      #expect(info.totalBytes > 0, "\(url.lastPathComponent)")
      #expect(info.articleCount > 0)
      #expect(!ReleaseName.looksObfuscated(info.title), "\(info.title)")
    }
  }

  static var realNZBs: [URL] {
    let downloads = FileManager.default.homeDirectoryForCurrentUser.appending(path: "Downloads")
    let names = (try? FileManager.default.contentsOfDirectory(atPath: downloads.path(percentEncoded: false))) ?? []
    return names.filter { $0.hasSuffix(".nzb") }.map { downloads.appending(path: $0) }
  }

  // MARK: Names

  @Test("Obfuscated and generic names are recognised")
  func obfuscation() {
    #expect(ReleaseName.looksObfuscated("4172R01e3H14n37E65f01G58y82y7191.mkv"))
    #expect(ReleaseName.looksObfuscated("a1b2c3d4e5f60718293a4b5c6d7e8f90"))
    #expect(!ReleaseName.looksObfuscated("Sintel.2010.2160p.UHD.BluRay"))
    #expect(!ReleaseName.looksObfuscated("Short123"))
    #expect(ReleaseName.isGeneric("download"))
    #expect(ReleaseName.isGeneric("123456"))
    #expect(!ReleaseName.isGeneric("The_Linux_Command_Line"))
    #expect(ReleaseName.best([nil, "4172R01e3H14n37E65f01G58y82y7191", "download", "Real.Name"], fallback: "x") == "Real.Name")
    #expect(ReleaseName.best(["  "], fallback: " ") == "Download")
  }

  @Test("Folder names lose slashes, colons and leading dots, and stay short")
  func folderNames() {
    #expect(ReleaseName.folderName("AC/DC: Live") == "AC-DC- Live")
    #expect(ReleaseName.folderName("..hidden") == "hidden")
    #expect(ReleaseName.folderName("") == "Download")
    #expect(ReleaseName.folderName(String(repeating: "é", count: 300)).utf8.count <= 200)
  }

  @Test("File kinds and content kinds")
  func kinds() {
    #expect(NzbFile.kind(forName: "x.part01.rar") == .archive)
    #expect(NzbFile.kind(forName: "x.r07") == .archive)
    #expect(NzbFile.kind(forName: "x.001") == .archive)
    #expect(NzbFile.kind(forName: "x.vol03+04.PAR2") == .par2)
    #expect(NzbFile.kind(forName: "x.nfo") == .other)
    #expect(NzbFile.kind(forName: "x.mkv") == .data)
    let files = [NzbFile(name: "a.flac", bytes: 300, segments: 1, kind: .data), NzbFile(name: "a.jpg", bytes: 10, segments: 1, kind: .data)]
    #expect(ContentKind.dominant(in: files) == .audio)
    #expect(ContentKind.archive.refined(byReleaseName: "Artist - Album (2020) [FLAC]") == .audio)
    #expect(ContentKind.archive.refined(byReleaseName: "Some.Book.EPUB") == .document)
    #expect(ContentKind.archive.refined(byReleaseName: "Show.S02E04.Name") == .video)
    #expect(ContentKind.archive.refined(byReleaseName: "Backup files") == .archive)
    #expect(ContentKind.audio.refined(byReleaseName: "Movie.1080p") == .audio)
  }

  // MARK: Errors and settings values

  @Test("Engine errors carry one sentence and know a server problem")
  func engineErrors() {
    for kind in EngineError.Kind.allCases {
      let error = EngineError(kind)
      #expect(error.errorDescription == kind.defaultMessage)
      #expect(error.message.hasSuffix("."))
    }
    #expect(EngineError(.auth, "  ").message == EngineError.Kind.auth.defaultMessage)
    #expect(EngineError(.io, "Disk said no.").localizedDescription == "Disk said no.")
    #expect(Set(EngineError.Kind.allCases.filter(\.isServerProblem)) == [.auth, .dns, .connect, .tls, .timeout])
  }

  @Test("Logged settings never show the password")
  func redaction() {
    let settings = EngineSettings(server: ServerSettings(host: "news.example.com"), password: "hunter2")
    #expect(!String(describing: settings).contains("hunter2"))
    #expect(!String(reflecting: settings).contains("hunter2"))
    var dumped = ""
    dump(settings, to: &dumped)
    #expect(!dumped.contains("hunter2"))
    let imported = ImportedSettings(server: ServerSettings(host: "h"), password: "hunter2")
    #expect(!String(describing: imported).contains("hunter2"))
  }

  @Test("A pasted nntps:// URL is a host")
  func normalisedHost() {
    #expect(ServerSettings(host: " nntps://news.example.com/ ").normalisedHost == "news.example.com")
    #expect(ServerSettings.defaultPort(useSSL: false) == 119)
  }

  // MARK: CLI config

  @Test("The CLI's config.toml becomes imported settings, password included")
  func cliConfig() throws {
    let toml = """
      # dl-nzb Configuration File
      [usenet]
      server = "news.example.com"
      port = 443
      username = "zeph"
      password = "p\\"ss # not a comment"
      ssl = true
      verify_ssl_certs = false
      connections = 50
      timeout = 30
      retry_attempts = 3
      retry_delay = 500

      [download]
      dir = "/Volumes/Media/Usenet"
      create_subfolders = true

      [post_processing]
      auto_par2_repair = true
      auto_extract_rar = false
      delete_rar_after_extract = true
      delete_par2_after_repair = true
      deobfuscate_file_names = false
      download_all_par2 = true

      [tuning]
      fsync_on_finalize = true # durable
      """
    let imported = try CLIConfig.parse(toml)
    #expect(imported.server.host == "news.example.com")
    #expect(imported.server.port == 443)
    #expect(imported.server.username == "zeph")
    #expect(imported.password == "p\"ss # not a comment")
    #expect(imported.server.useSSL && !imported.server.verifyCertificate)
    #expect(imported.server.connections == 50 && imported.server.retryAttempts == 3)
    #expect(
      imported.processing
        == ProcessingSettings(
          repairWithPar2: true, extractArchives: false, deleteArchivesAfterExtracting: true, deletePar2AfterRepairing: true, renameObfuscatedFiles: false))
    #expect(imported.advanced.downloadAllRecoveryUpFront && imported.advanced.flushFilesWhenFinished)
    #expect(imported.downloadDirectory == URL(filePath: "/Volumes/Media/Usenet", directoryHint: .isDirectory))
  }

  @Test("A CLI config without a server is not worth importing, and missing keys take defaults")
  func cliConfigDefaults() throws {
    #expect(throws: EngineError.self) { try CLIConfig.parse("[usenet]\nserver = \"\"\n") }
    let imported = try CLIConfig.parse("[usenet]\nserver = 'news.example.com'\nssl = false\n[download]\ndir = \"downloads\"\n")
    #expect(imported.server.port == 119)
    #expect(imported.server.connections == ServerSettings.defaultConnections)
    #expect(imported.processing == ProcessingSettings())
    #expect(imported.downloadDirectory == nil)
  }

  @Test("Reading a config file records where it came from")
  func cliConfigFile() throws {
    let scratch = Scratch()
    let url = scratch.url.appending(path: "config.toml")
    try Data("[usenet]\nserver = \"news.example.com\"\n".utf8).write(to: url)
    #expect(try CLIConfig.read(from: url).source == url)
    #expect(throws: EngineError.self) { try CLIConfig.read(from: scratch.url.appending(path: "missing.toml")) }
  }

  @Test("A progress's display fraction is bytes while moving bytes, else the phase's")
  func displayFraction() {
    #expect(JobProgress(phase: .downloading, bytesDone: 25, bytesTotal: 100, fraction: 0.9).displayFraction == 0.25)
    #expect(JobProgress(phase: .verifying, bytesDone: 25, bytesTotal: 100, fraction: 0.9).displayFraction == 0.9)
    #expect(JobProgress(phase: .downloading, fraction: 2).displayFraction == 1)
  }

  @Test("Stop asks about data only once downloading has begun, in this run or an earlier one")
  func hasData() {
    var item = PreviewData.connecting
    item.visitedPhases = [.connecting]
    #expect(!item.hasData)
    #expect(!PreviewData.queued.hasData)
    #expect(PreviewData.downloading.hasData)
    #expect(PreviewData.paused.hasData)
    #expect(PreviewData.extracting.hasData)
    // Connecting again after an earlier run downloaded part of it.
    item.progress = JobProgress(phase: .connecting)
    item.visitedPhases = [.connecting, .checking, .downloading]
    #expect(item.hasData)
    // Cancelled with its data deleted.
    item.progress = nil
    #expect(!item.hasData)
  }

  @Test("Remove from List asks first for a running job or one with data, never for a finished download")
  func removalConfirmation() {
    #expect(PreviewData.downloading.needsRemovalConfirmation)
    #expect(PreviewData.connecting.needsRemovalConfirmation)
    #expect(PreviewData.extracting.needsRemovalConfirmation)
    #expect(PreviewData.paused.needsRemovalConfirmation)
    #expect(PreviewData.stopped.needsRemovalConfirmation)
    #expect(PreviewData.needsPassword.needsRemovalConfirmation)
    #expect(!PreviewData.queued.needsRemovalConfirmation)
    #expect(!PreviewData.needsAttentionUnrepairable.needsRemovalConfirmation)
    #expect(!PreviewData.finished.needsRemovalConfirmation)
    #expect(!PreviewData.finishedWithIssues.needsRemovalConfirmation)
    var stoppedAndDeleted = PreviewData.stopped
    stoppedAndDeleted.progress = nil
    #expect(!stoppedAndDeleted.needsRemovalConfirmation)
  }

  @Test("A release's size is its data, without the recovery files")
  func sizes() {
    let item = PreviewData.queued
    #expect(item.totalBytes == PreviewData.tearsOfSteelInfo.dataBytes)
    var bare = item
    bare.info?.dataBytes = 0
    #expect(bare.totalBytes == PreviewData.tearsOfSteelInfo.totalBytes)
    bare.info = nil
    #expect(bare.totalBytes == 0)
  }

  @Test("A second copy's row name says which it is; the release name does not change")
  func displayTitle() {
    var item = PreviewData.queued
    #expect(item.displayTitle == item.title)
    item.copyNumber = 3
    #expect(item.displayTitle == "\(PreviewData.tearsOfSteelTitle) (3)")
    #expect(item.title == PreviewData.tearsOfSteelTitle)
  }

  @Test("A list saved before copy numbers existed still loads")
  func decodesWithoutCopyNumber() throws {
    let data = try JSONEncoder().encode(PreviewData.queued)
    var object = try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
    object.removeValue(forKey: "copyNumber")
    let decoded = try JSONDecoder().decode(DownloadItem.self, from: JSONSerialization.data(withJSONObject: object))
    #expect(decoded.copyNumber == nil && decoded.title == PreviewData.queued.title)
  }

  @Test("Listed files leave out recovery data, biggest first, then by name")
  func listedFiles() {
    let item = PreviewData.queued
    let listed = item.listedFiles
    #expect(listed.allSatisfy { NzbFile.kind(forName: $0.name) != .par2 })
    #expect(listed.count == (item.info?.files.count(where: { $0.kind != .par2 }) ?? 0))
    #expect(zip(listed, listed.dropFirst()).allSatisfy { $0.bytes > $1.bytes || ($0.bytes == $1.bytes && $0.name < $1.name) })
    #expect(PreviewData.finished.listedFiles == PreviewData.finishedSummary.files)
  }
}
