// Records a region of the screen around an app's window, for compose.py.
//
//   record-mac --app <bundle id> --out TAKE_DIR [--include <bundle id>]... [--margin 0]
//              [--include-window <bundle id>=<title part>]... [--region x,y,w,h] [--duration seconds]
//
// The region is the app's largest window (plus --margin points), or --region in
// global points. It is captured at the display's own scale (2x on a Retina
// display) with ScreenCaptureKit into TAKE_DIR/take.mov (HEVC), until SIGINT,
// SIGTERM or --duration. Only the app and the --include apps are drawn, and of
// an --include-window app only the windows whose title has one of the given
// parts (the take's own Finder windows, not the owner's others or the desktop);
// everything else is left out, so the background is black. The filter is a
// display filter that excludes the other apps: an app-only filter makes macOS
// 26 draw a sharing pill in place of the window's traffic lights.
//
// Also writes TAKE_DIR/take.json (when the first frame was captured, in epoch
// seconds, and the region) and TAKE_DIR/windows.jsonl: the frames of the
// recorded apps' windows over time, in points from the region's top left,
// which compose.py turns into the window mask.
//
// Needs Screen Recording permission for the terminal it runs from.

import AVFoundation
import AppKit
import CoreMedia
import Foundation
import ScreenCaptureKit

struct Options {
  var app = ""
  var includes: [String] = []
  /// Bundle id to title parts: only those windows of the app are recorded.
  var titled: [String: [String]] = [:]
  var out = ""
  var margin: CGFloat = 0
  var region: CGRect?
  var duration: Double?

  init() {
    var args = CommandLine.arguments.dropFirst().makeIterator()
    while let arg = args.next() {
      switch arg {
      case "--app": app = args.next() ?? ""
      case "--include": includes.append(args.next() ?? "")
      case "--include-window":
        let pair = (args.next() ?? "").split(separator: "=", maxSplits: 1).map(String.init)
        if pair.count == 2 { titled[pair[0], default: []].append(pair[1]) }
      case "--out": out = args.next() ?? ""
      case "--margin": margin = CGFloat(Double(args.next() ?? "") ?? 0)
      case "--duration": duration = Double(args.next() ?? "")
      case "--region":
        let parts = (args.next() ?? "").split(separator: ",").compactMap { Double($0) }
        if parts.count == 4 { region = CGRect(x: parts[0], y: parts[1], width: parts[2], height: parts[3]) }
      default: fail("unknown argument \(arg)")
      }
    }
    if app.isEmpty || out.isEmpty { fail("usage: record-mac --app <bundle id> --out TAKE_DIR [--include <bundle id>]... [--margin pt] [--region x,y,w,h] [--duration s]") }
  }
}

func fail(_ message: String) -> Never {
  FileHandle.standardError.write(Data((message + "\n").utf8))
  exit(1)
}

/// On-screen windows of these processes or apps (by name, so a relaunched app
/// counts at once, before the process list catches up), in global points
/// (top-left origin), plus the `windows` given by number.
func windowList(pids: Set<pid_t>, names: Set<String> = [], windows: Set<CGWindowID> = []) -> [(frame: CGRect, layer: Int, pid: pid_t)] {
  guard let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else { return [] }
  var result: [(CGRect, Int, pid_t)] = []
  for entry in info {
    guard let pid = entry[kCGWindowOwnerPID as String] as? pid_t else { continue }
    let number = entry[kCGWindowNumber as String] as? CGWindowID ?? 0
    let owner = entry[kCGWindowOwnerName as String] as? String ?? ""
    guard pids.contains(pid) || names.contains(owner) || windows.contains(number) else { continue }
    let layer = entry[kCGWindowLayer as String] as? Int ?? 0
    let alpha = entry[kCGWindowAlpha as String] as? Double ?? 1
    // Ordinary windows, sheets, popovers and menus; not the desktop, the
    // menu bar's status items (25) or the Dock's level and above.
    guard layer >= 0, layer != 25, layer < 1000, alpha > 0.01 else { continue }
    guard let bounds = entry[kCGWindowBounds as String] as? NSDictionary, let frame = CGRect(dictionaryRepresentation: bounds) else { continue }
    if frame.width < 4 || frame.height < 4 { continue }
    result.append((frame, layer, pid))
  }
  return result
}

final class Recorder: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
  let options: Options
  let outDir: URL
  var stream: SCStream?
  var writer: AVAssetWriter!
  var input: AVAssetWriterInput!
  var region = CGRect.zero
  var scale: CGFloat = 2
  var pids = Set<pid_t>()
  /// The recorded apps' names, for windows of a copy launched during the take.
  var names = Set<String>()
  /// The --include-window windows recorded right now (the filter's and the mask's).
  var titledWindows = Set<CGWindowID>()
  var bundleIDs = Set<String>()
  var display: SCDisplay!
  // SCStreamConfiguration does not retain its background colour: keep it, or
  // creating the stream crashes.
  let background = CGColor(red: 0, green: 0, blue: 0, alpha: 1)
  var firstFrameEpoch: Double?
  var started = false
  var frames = 0
  var lastWindows = ""
  var windowLog: FileHandle!
  let queue = DispatchQueue(label: "record-mac.frames")
  var timers: [DispatchSourceTimer] = []

  init(options: Options) {
    self.options = options
    self.outDir = URL(fileURLWithPath: options.out, isDirectory: true)
  }

  func start() async throws {
    try FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)
    bundleIDs = Set([options.app] + options.includes)
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    let recorded = content.applications.filter { bundleIDs.contains($0.bundleIdentifier) }
    pids = Set(recorded.map(\.processID))
    names = Set(recorded.map(\.applicationName).filter { !$0.isEmpty })
    guard let target = content.applications.first(where: { $0.bundleIdentifier == options.app }) else { fail("\(options.app) is not running") }

    if let given = options.region {
      region = given
    } else {
      let windows = windowList(pids: [target.processID]).filter { $0.layer == 0 }
      guard let main = windows.max(by: { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height }) else { fail("\(options.app) has no window on screen") }
      region = main.frame.insetBy(dx: -options.margin, dy: -options.margin)
    }
    region = region.integral
    let center = CGPoint(x: region.midX, y: region.midY)
    guard let display = content.displays.first(where: { $0.frame.contains(center) }) ?? content.displays.first else { fail("no display") }
    self.display = display
    scale = NSScreen.screens.first { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == display.displayID }?.backingScaleFactor ?? 2

    let config = SCStreamConfiguration()
    config.sourceRect = CGRect(x: region.minX - display.frame.minX, y: region.minY - display.frame.minY, width: region.width, height: region.height)
    config.width = Int(region.width * scale)
    config.height = Int(region.height * scale)
    config.scalesToFit = false
    config.showsCursor = true
    config.minimumFrameInterval = CMTime(value: 1, timescale: 60)
    config.pixelFormat = kCVPixelFormatType_32BGRA
    config.colorSpaceName = CGColorSpace.sRGB
    config.backgroundColor = background
    config.queueDepth = 8
    config.capturesAudio = false

    let movie = outDir.appendingPathComponent("take.mov")
    try? FileManager.default.removeItem(at: movie)
    writer = try AVAssetWriter(outputURL: movie, fileType: .mov)
    input = AVAssetWriterInput(
      mediaType: .video,
      outputSettings: [
        AVVideoCodecKey: AVVideoCodecType.hevc,
        AVVideoWidthKey: config.width,
        AVVideoHeightKey: config.height,
        AVVideoColorPropertiesKey: [
          AVVideoColorPrimariesKey: AVVideoColorPrimaries_ITU_R_709_2,
          AVVideoTransferFunctionKey: AVVideoTransferFunction_ITU_R_709_2,
          AVVideoYCbCrMatrixKey: AVVideoYCbCrMatrix_ITU_R_709_2,
        ],
        AVVideoCompressionPropertiesKey: [
          AVVideoAverageBitRateKey: 40_000_000,
          AVVideoExpectedSourceFrameRateKey: 60,
          AVVideoAllowFrameReorderingKey: false,
        ],
      ])
    input.expectsMediaDataInRealTime = true
    writer.add(input)
    guard writer.startWriting() else { fail("could not write \(movie.path): \(writer.error?.localizedDescription ?? "")") }

    FileManager.default.createFile(atPath: outDir.appendingPathComponent("windows.jsonl").path, contents: nil)
    windowLog = try FileHandle(forWritingTo: outDir.appendingPathComponent("windows.jsonl"))

    titledWindows = Set(matchingWindows(content).map(\.windowID))
    let stream = SCStream(filter: filter(content), configuration: config, delegate: self)
    try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
    try await stream.startCapture()
    self.stream = stream
    logWindows()
    every(0.05) { [weak self] in self?.logWindows() }
    // Apps that launch during the take (a notification, say) are left out too,
    // and a matching window that opens (Show in Finder) is added quickly.
    every(options.titled.isEmpty ? 1.0 : 0.2) { [weak self] in self?.refreshFilter() }
    print("recording \(Int(region.width))x\(Int(region.height)) pt at \(Int(region.minX)),\(Int(region.minY)), \(config.width)x\(config.height) px")
  }

  /// Every app but the recorded ones is left out; an --include-window app is
  /// left out too, but for its matching windows.
  func filter(_ content: SCShareableContent) -> SCContentFilter {
    let others = content.applications.filter { !bundleIDs.contains($0.bundleIdentifier) }
    return SCContentFilter(display: display, excludingApplications: others, exceptingWindows: matchingWindows(content))
  }

  func matchingWindows(_ content: SCShareableContent) -> [SCWindow] {
    content.windows.filter { window in
      guard window.isOnScreen, let id = window.owningApplication?.bundleIdentifier, let parts = options.titled[id], let title = window.title else { return false }
      return parts.contains { title.contains($0) }
    }
  }

  func refreshFilter() {
    Task {
      guard let content = try? await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true) else { return }
      let newPids = Set(content.applications.filter { bundleIDs.contains($0.bundleIdentifier) }.map(\.processID))
      let newWindows = Set(matchingWindows(content).map(\.windowID))
      try? await stream?.updateContentFilter(filter(content))
      queue.async {
        self.pids = newPids
        self.titledWindows = newWindows
      }
    }
  }

  func every(_ seconds: Double, _ body: @escaping () -> Void) {
    let timer = DispatchSource.makeTimerSource(queue: queue)
    timer.schedule(deadline: .now() + seconds, repeating: seconds)
    timer.setEventHandler(handler: body)
    timer.resume()
    timers.append(timer)
  }

  /// One line whenever the recorded apps' windows inside the region change.
  func logWindows() {
    var rows: [[Double]] = []
    for window in windowList(pids: pids, names: names, windows: titledWindows) {
      let clipped = window.frame.intersection(region)
      if clipped.isNull || clipped.width < 4 || clipped.height < 4 { continue }
      // Unclipped frame, relative to the region: the mask rounds its real corners.
      let f = window.frame.offsetBy(dx: -region.minX, dy: -region.minY)
      rows.append([f.minX, f.minY, f.width, f.height, Double(window.layer)].map { ($0 * 10).rounded() / 10 })
    }
    // Front to back from the window server; the mask is a union, so sort for a stable signature.
    rows.sort { $0.lexicographicallyPrecedes($1) }
    guard let data = try? JSONSerialization.data(withJSONObject: rows), let json = String(data: data, encoding: .utf8), json != lastWindows else { return }
    lastWindows = json
    let line = "{\"t\": \(String(format: "%.3f", Date().timeIntervalSince1970)), \"windows\": \(json)}\n"
    windowLog.write(Data(line.utf8))
  }

  func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
    guard type == .screen, sampleBuffer.isValid else { return }
    guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
      let raw = attachments.first?[.status] as? Int, SCFrameStatus(rawValue: raw) == .complete
    else { return }
    let pts = sampleBuffer.presentationTimeStamp
    if !started {
      let hostNow = CMClockGetTime(CMClockGetHostTimeClock()).seconds
      firstFrameEpoch = Date().timeIntervalSince1970 - (hostNow - pts.seconds)
      writer.startSession(atSourceTime: pts)
      started = true
    }
    if input.isReadyForMoreMediaData {
      input.append(sampleBuffer)
      frames += 1
    }
  }

  func stream(_ stream: SCStream, didStopWithError error: any Error) {
    FileHandle.standardError.write(Data("capture stopped: \(error.localizedDescription)\n".utf8))
    Task { await self.stop() }
  }

  func stop() async {
    for timer in timers { timer.cancel() }
    try? await stream?.stopCapture()
    stream = nil
    await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
      queue.async {
        self.input.markAsFinished()
        self.writer.finishWriting { done.resume() }
      }
    }
    try? windowLog.close()
    let info: [String: Any] = [
      "kind": "mac", "movie": "take.mov", "start": firstFrameEpoch ?? 0, "scale": Double(scale),
      "region": ["x": region.minX, "y": region.minY, "w": region.width, "h": region.height],
      "app": options.app, "frames": frames,
    ]
    let data = try! JSONSerialization.data(withJSONObject: info, options: [.prettyPrinted, .sortedKeys])
    try? data.write(to: outDir.appendingPathComponent("take.json"))
    print("wrote \(frames) frames to \(outDir.appendingPathComponent("take.mov").path)")
    exit(writer.status == .completed ? 0 : 1)
  }
}

let recorder = Recorder(options: Options())
signal(SIGINT, SIG_IGN)
signal(SIGTERM, SIG_IGN)
var signalSources: [DispatchSourceSignal] = []
for sig in [SIGINT, SIGTERM] {
  let source = DispatchSource.makeSignalSource(signal: sig, queue: .main)
  source.setEventHandler { Task { await recorder.stop() } }
  source.resume()
  signalSources.append(source)
}
Task {
  do {
    try await recorder.start()
  } catch {
    fail("could not start recording: \(error.localizedDescription)")
  }
  if let duration = recorder.options.duration {
    try? await Task.sleep(for: .seconds(duration))
    await recorder.stop()
  }
}
NSApplication.shared.setActivationPolicy(.prohibited)
RunLoop.main.run()
