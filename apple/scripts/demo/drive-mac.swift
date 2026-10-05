// Drives a Mac app through the steps of a storyboard's `take`, like a person
// at the mouse, and notes when each `mark` step ran.
//
//   drive-mac --storyboard mac.json --out TAKE_DIR      run take.steps, write TAKE_DIR/marks.json
//   drive-mac --storyboard mac.json --out TAKE_DIR --steps setup   run take.setup instead
//   drive-mac --dump <bundle id>                         list the app's buttons, rows and text, with frames
//   drive-mac --bookmark <folder>                        a bookmark for -downloadFolderBookmark, as <hex>
//
// The pointer glides with an ease-in-out curve and clicks are real CGEvents,
// so the recording shows the cursor doing what a person would. Targets are
// found through Accessibility by their label, so a storyboard survives layout
// changes.
//
// Safety: before every step that moves the pointer, clicks, types or changes
// the frontmost app, it stops if someone is using the Mac: input newer than
// its own last event, by HIDIdleTime. (On macOS 26 posted events reset
// HIDIdleTime too, so the idle time alone cannot tell them apart.) After each
// glide it checks the pointer is where it put it. Before every click and key
// it checks that the frontmost app is the one the step expects, and stops if
// not, so nothing lands in another app. Needs Accessibility permission for the
// terminal.
//
// Steps, as JSON arrays (`app` is take.app unless a step names another):
//   ["wait", 1.5]                       seconds
//   ["mark", "name"]                    note the time for the cut
//   ["activate"] / ["activate", "com.apple.finder"]
//   ["launch", ["-simulate", "YES"]]    open the app (--set app=/path/dl-nzb.app) with these arguments
//   ["open", "/path/a.nzb"]             open a file with the app, as a double-click in Finder does
//   ["reveal", "/path/folder", [x, y, w, h]]   a Finder window on the folder, placed in points
//                                       relative to the app's window (negative x: to its left)
//   ["move", TARGET, 0.8]               glide the pointer to TARGET over 0.8 s
//   ["click", TARGET], ["double-click", TARGET], ["right-click", TARGET]
//   ["key", "cmd+q"]                    a shortcut: cmd, shift, option, ctrl + a key
//   ["type", "text"]
//   ["press", TARGET]                   AXPress, no pointer
//   ["wait-for", TARGET, 60]            until TARGET exists (a row that reads "Finished", say)
//   ["wait-gone", TARGET, 60]           until TARGET is gone ("downloading" in the window's subtitle, say)
//   ["wait-file", "/path", 60]          until a file exists
//   ["window", [1040, 680]]             the app's window at this size, centred (or [x, y, w, h])
//   ["window-of", "com.apple.finder", [x, y, w, h]]  another app's front window, relative to the app's
// Strings may hold "{take}" (TAKE_DIR) and any "{name}" given with --set name=value.
// TARGET: {"label": "Add NZB"}            title, description, identifier or help equal to this
//         {"text": "Finished", "role": "AXStaticText"}   a value or title containing this
//         {"app": "com.apple.finder", "text": "Sintel.2010.2160p.UHD.BluRay.x265.nzb"}
//         {"window": [0.5, 0.2]}          a point as a share of the app's window
//         {"point": [120, 40]}            points from the app's window's top left
//         add "index": 2 for the third match (rows list top to bottom)

import AppKit
import ApplicationServices
import Foundation
import IOKit

func fail(_ message: String, code: Int32 = 1) -> Never {
  FileHandle.standardError.write(Data(("drive-mac: " + message + "\n").utf8))
  exit(code)
}

func say(_ message: String) {
  FileHandle.standardError.write(Data((message + "\n").utf8))
}

// MARK: Safety

/// Seconds since the last real keyboard, mouse or trackpad input.
func hidIdleSeconds() -> Double {
  let service = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("IOHIDSystem"))
  defer { IOObjectRelease(service) }
  guard service != 0, let value = IORegistryEntryCreateCFProperty(service, "HIDIdleTime" as CFString, kCFAllocatorDefault, 0)?.takeRetainedValue() else { return 0 }
  let nanoseconds = (value as? NSNumber)?.uint64Value ?? 0
  return Double(nanoseconds) / 1e9
}

var minimumIdle = 2.0
/// When this tool last posted an event.
var lastPost = Date.distantPast

/// Someone (a person, or another tool) has used the keyboard or mouse in the
/// last `minimumIdle` seconds, after this tool's own last event.
func someoneElseActive() -> Bool {
  let idle = hidIdleSeconds()
  return idle < minimumIdle && idle + 0.3 < Date().timeIntervalSince(lastPost)
}

/// Stops the take if somebody else has touched the keyboard or mouse in the
/// last `minimumIdle` seconds: the Mac is theirs, and the take can be redone.
func waitForIdle() {
  if someoneElseActive() { fail("the keyboard or mouse is in use; stopped the take", code: 3) }
}

/// Stops the take unless `bundleID` is the frontmost app (allowing a moment
/// for an activation that is on its way).
func requireFrontmost(_ bundleID: String, within seconds: Double = 3) {
  for _ in 0..<Int(seconds * 10) {
    if NSWorkspace.shared.frontmostApplication?.bundleIdentifier == bundleID { return }
    // Run the run loop, not sleep: NSWorkspace learns of a new frontmost app
    // through it, and a tool that only sleeps keeps seeing the old one.
    RunLoop.current.run(until: Date().addingTimeInterval(0.1))
  }
  let front = NSWorkspace.shared.frontmostApplication?.bundleIdentifier ?? "nothing"
  fail("\(front) is frontmost, not \(bundleID); stopped so nothing lands in the wrong app", code: 4)
}

// MARK: Accessibility

func running(_ bundleID: String) -> NSRunningApplication? {
  NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).first
}

func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
  var value: CFTypeRef?
  return AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success ? value : nil
}

func string(_ element: AXUIElement, _ name: String) -> String {
  guard let value = attribute(element, name) else { return "" }
  if let text = value as? String { return text }
  if CFGetTypeID(value) == CFNumberGetTypeID() || CFGetTypeID(value) == CFBooleanGetTypeID() { return "\(value)" }
  return ""
}

func children(_ element: AXUIElement) -> [AXUIElement] {
  (attribute(element, kAXChildrenAttribute) as? [AXUIElement]) ?? []
}

func frame(_ element: AXUIElement) -> CGRect? {
  guard let position = attribute(element, kAXPositionAttribute), let size = attribute(element, kAXSizeAttribute) else { return nil }
  var point = CGPoint.zero
  var extent = CGSize.zero
  AXValueGetValue(position as! AXValue, .cgPoint, &point)
  AXValueGetValue(size as! AXValue, .cgSize, &extent)
  return CGRect(origin: point, size: extent)
}

func windows(of bundleID: String) -> [AXUIElement] {
  guard let app = running(bundleID) else { return [] }
  let element = AXUIElementCreateApplication(app.processIdentifier)
  return (attribute(element, kAXWindowsAttribute) as? [AXUIElement]) ?? []
}

func largestWindow(_ bundleID: String) -> AXUIElement? {
  windows(of: bundleID).max { (frame($0)?.width ?? 0) * (frame($0)?.height ?? 0) < (frame($1)?.width ?? 0) * (frame($1)?.height ?? 0) }
}

func setFrame(of window: AXUIElement?, _ rect: CGRect) {
  guard let window else { fail("no window to place") }
  var origin = rect.origin
  var size = rect.size
  if let extent = AXValueCreate(.cgSize, &size) { AXUIElementSetAttributeValue(window, kAXSizeAttribute as CFString, extent) }
  if let position = AXValueCreate(.cgPoint, &origin) { AXUIElementSetAttributeValue(window, kAXPositionAttribute as CFString, position) }
}

/// The app's largest window.
func mainWindowFrame(_ bundleID: String) -> CGRect? {
  windows(of: bundleID).compactMap(frame).max { $0.width * $0.height < $1.width * $1.height }
}

struct Target {
  var app: String
  var label: String?
  var text: String?
  var role: String?
  var window: [Double]?
  var point: [Double]?
  /// Which match, counting from 0 in the order Accessibility lists them.
  var index = 0

  init(_ value: Any?, defaultApp: String) {
    let spec = value as? [String: Any] ?? [:]
    app = spec["app"] as? String ?? defaultApp
    label = spec["label"] as? String
    text = spec["text"] as? String
    role = spec["role"] as? String
    window = spec["window"] as? [Double]
    point = spec["point"] as? [Double]
    index = spec["index"] as? Int ?? 0
    if value is String { label = value as? String }
  }

  var isEmpty: Bool { label == nil && text == nil && window == nil && point == nil }

  func matches(_ element: AXUIElement) -> Bool {
    let elementRole = string(element, kAXRoleAttribute)
    if let role, elementRole != role { return false }
    if let label {
      let names = [kAXTitleAttribute, kAXDescriptionAttribute, kAXIdentifierAttribute, kAXHelpAttribute].map { string(element, $0) }
      return names.contains(label)
    }
    if let text {
      return [kAXValueAttribute, kAXTitleAttribute, kAXDescriptionAttribute].map { string(element, $0) }.contains { $0.contains(text) }
    }
    return false
  }

  func element() -> AXUIElement? {
    var found: AXUIElement?
    var skipped = 0
    func walk(_ element: AXUIElement, _ depth: Int) {
      if found != nil || depth > 40 { return }
      for child in children(element) {
        if matches(child), let f = frame(child), f.width > 0 {
          if skipped == index {
            found = child
            return
          }
          skipped += 1
        }
        walk(child, depth + 1)
      }
    }
    for window in windows(of: app) { walk(window, 0) }
    return found
  }

  /// Where the pointer should go, in global points.
  func location() -> CGPoint? {
    if let window, let f = mainWindowFrame(app) { return CGPoint(x: f.minX + window[0] * f.width, y: f.minY + window[1] * f.height) }
    if let point, let f = mainWindowFrame(app) { return CGPoint(x: f.minX + point[0], y: f.minY + point[1]) }
    guard let element = element(), let f = frame(element) else { return nil }
    return CGPoint(x: f.midX, y: f.midY)
  }

  var description: String { label ?? text ?? window.map { "\($0)" } ?? point.map { "\($0)" } ?? "?" }
}

// MARK: Pointer and keys

func pointer() -> CGPoint { CGEvent(source: nil)?.location ?? .zero }

/// Events from the HID system's state, so a held button counts as held.
let eventSource = CGEventSource(stateID: .hidSystemState)

func post(_ type: CGEventType, _ point: CGPoint, button: CGMouseButton = .left, clicks: Int64 = 1) {
  let event = CGEvent(mouseEventSource: eventSource, mouseType: type, mouseCursorPosition: point, mouseButton: button)
  event?.setIntegerValueField(.mouseEventClickState, value: clicks)
  event?.post(tap: .cghidEventTap)
  lastPost = Date()
}

/// Glides the pointer along an ease-in-out curve at 120 Hz.
func glide(to end: CGPoint, seconds: Double) {
  let start = pointer()
  let steps = max(1, Int(seconds * 120))
  for step in 1...steps {
    let t = Double(step) / Double(steps)
    let e = t < 0.5 ? 4 * t * t * t : 1 - pow(-2 * t + 2, 3) / 2
    post(.mouseMoved, CGPoint(x: start.x + (end.x - start.x) * e, y: start.y + (end.y - start.y) * e))
    Thread.sleep(forTimeInterval: seconds / Double(steps))
  }
  Thread.sleep(forTimeInterval: 0.03)
  let now = pointer()
  if hypot(now.x - end.x, now.y - end.y) > 3 { fail("the pointer moved while gliding: someone is using the mouse", code: 3) }
}

func click(at point: CGPoint, count: Int = 1, right: Bool = false) {
  let (down, up, button): (CGEventType, CGEventType, CGMouseButton) = right ? (.rightMouseDown, .rightMouseUp, .right) : (.leftMouseDown, .leftMouseUp, .left)
  for n in 1...count {
    post(down, point, button: button, clicks: Int64(n))
    Thread.sleep(forTimeInterval: 0.06)
    post(up, point, button: button, clicks: Int64(n))
    Thread.sleep(forTimeInterval: 0.09)
  }
}

let keyCodes: [String: CGKeyCode] = [
  "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14, "r": 15,
  "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "=": 24, "9": 25, "7": 26, "-": 27, "8": 28, "0": 29,
  "]": 30, "o": 31, "u": 32, "[": 33, "i": 34, "p": 35, "return": 36, "l": 37, "j": 38, "'": 39, "k": 40, ";": 41, "\\": 42,
  ",": 43, "/": 44, "n": 45, "m": 46, ".": 47, "tab": 48, "space": 49, "delete": 51, "escape": 53, "left": 123, "right": 124,
  "down": 125, "up": 126,
]

func key(_ combo: String) {
  var flags: CGEventFlags = []
  var name = ""
  for part in combo.lowercased().split(separator: "+").map(String.init) {
    switch part {
    case "cmd", "command": flags.insert(.maskCommand)
    case "shift": flags.insert(.maskShift)
    case "option", "alt": flags.insert(.maskAlternate)
    case "ctrl", "control": flags.insert(.maskControl)
    default: name = part
    }
  }
  guard let code = keyCodes[name] else { fail("unknown key \(name)") }
  for down in [true, false] {
    let event = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down)
    event?.flags = flags
    event?.post(tap: .cghidEventTap)
    lastPost = Date()
    Thread.sleep(forTimeInterval: 0.05)
  }
}

func type(_ text: String) {
  for character in text {
    for down in [true, false] {
      let event = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: down)
      var units = Array(String(character).utf16)
      event?.keyboardSetUnicodeString(stringLength: units.count, unicodeString: &units)
      event?.post(tap: .cghidEventTap)
      lastPost = Date()
    }
    Thread.sleep(forTimeInterval: 0.07)
  }
}

// MARK: Steps

func activate(_ bundleID: String) {
  waitForIdle()
  guard let app = running(bundleID) else { fail("\(bundleID) is not running") }
  app.activate()
  // Finder can take a few seconds to come forward after opening a folder.
  requireFrontmost(bundleID, within: 10)
}

func resolve(_ target: Target, timeout: Double = 5) -> CGPoint {
  let deadline = Date().addingTimeInterval(timeout)
  while true {
    if let point = target.location() { return point }
    if Date() > deadline { fail("could not find \(target.description) in \(target.app)") }
    Thread.sleep(forTimeInterval: 0.1)
  }
}

/// Puts a Finder window on `folder` at `rect` (points, relative to the app's
/// window). Through Accessibility, so no Automation prompt.
func reveal(_ folder: String, _ rect: [Double], app: String) {
  waitForIdle()
  NSWorkspace.shared.open(URL(fileURLWithPath: folder, isDirectory: true))
  let name = (folder as NSString).lastPathComponent
  var window: AXUIElement?
  for _ in 0..<50 {
    // The folder's name, or its whole path when Finder shows paths in titles.
    window = windows(of: "com.apple.finder").first {
      let title = string($0, kAXTitleAttribute)
      return title == name || title.hasSuffix("/" + name)
    }
    if window != nil { break }
    Thread.sleep(forTimeInterval: 0.1)
  }
  guard let window, let anchor = mainWindowFrame(app) else { fail("no Finder window for \(folder)") }
  setFrame(of: window, CGRect(x: anchor.minX + rect[0], y: anchor.minY + rect[1], width: rect[2], height: rect[3]))
}

/// Marks are saved as they are made, so a take that stops early still has them.
func saveMarks(_ marks: [String: Double], to url: URL) {
  try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
  if let json = try? JSONSerialization.data(withJSONObject: marks, options: [.prettyPrinted, .sortedKeys]) { try? json.write(to: url) }
}

/// "{take}" and the storyboard's other placeholders, in every string of a step.
var placeholders: [String: String] = [:]

func substitute(_ value: Any) -> Any {
  if var text = value as? String {
    for (key, replacement) in placeholders { text = text.replacingOccurrences(of: "{\(key)}", with: replacement) }
    return text
  }
  if let list = value as? [Any] { return list.map(substitute) }
  if let map = value as? [String: Any] { return map.mapValues(substitute) }
  return value
}

func run(steps: [[Any]], app: String, marks: inout [String: Double], marksURL: URL) {
  for rawStep in steps {
    let step = rawStep.map(substitute)
    guard let op = step.first as? String else { fail("bad step \(step)") }
    let args = Array(step.dropFirst())
    let shown = (try? JSONSerialization.data(withJSONObject: args)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
    say("· \(op) \(shown)")
    switch op {
    case "wait":
      // Watching for someone at the Mac the whole time, not only before a step.
      let until = Date().addingTimeInterval((args.first as? NSNumber)?.doubleValue ?? 1)
      while Date() < until {
        waitForIdle()
        Thread.sleep(forTimeInterval: min(0.1, max(0, until.timeIntervalSinceNow)))
      }
    case "mark":
      marks[args.first as? String ?? "mark"] = Date().timeIntervalSince1970
      saveMarks(marks, to: marksURL)
    case "activate":
      activate(args.first as? String ?? app)
    case "launch":
      waitForIdle()
      // The copy given with --set app=/path/dl-nzb.app, else whichever Launch Services knows.
      let given = placeholders["app"].map { URL(fileURLWithPath: $0) }
      guard let url = given ?? NSWorkspace.shared.urlForApplication(withBundleIdentifier: app) else { fail("\(app) is not installed") }
      let configuration = NSWorkspace.OpenConfiguration()
      configuration.arguments = args.first as? [String] ?? []
      configuration.activates = true
      let done = DispatchSemaphore(value: 0)
      NSWorkspace.shared.openApplication(at: url, configuration: configuration) { _, error in
        if let error { say("launch: \(error.localizedDescription)") }
        done.signal()
      }
      done.wait()
    case "open":
      waitForIdle()
      let given = placeholders["app"].map { URL(fileURLWithPath: $0) }
      guard let path = args.first as? String, let url = given ?? NSWorkspace.shared.urlForApplication(withBundleIdentifier: app) else { fail("open needs a path") }
      let done = DispatchSemaphore(value: 0)
      NSWorkspace.shared.open([URL(fileURLWithPath: path)], withApplicationAt: url, configuration: NSWorkspace.OpenConfiguration()) { _, _ in done.signal() }
      done.wait()
    case "reveal":
      reveal(args[0] as? String ?? "", (args[1] as? [NSNumber] ?? []).map(\.doubleValue), app: app)
    case "move":
      let target = Target(args.first, defaultApp: app)
      let point = resolve(target)
      waitForIdle()
      glide(to: point, seconds: (args.dropFirst().first as? NSNumber)?.doubleValue ?? 0.8)
    case "click", "double-click", "right-click":
      let target = Target(args.first, defaultApp: app)
      if !target.isEmpty {
        let point = resolve(target)
        waitForIdle()
        glide(to: point, seconds: (args.dropFirst().first as? NSNumber)?.doubleValue ?? 0.8)
        Thread.sleep(forTimeInterval: 0.15)
      }
      waitForIdle()
      requireFrontmost(target.app)
      click(at: pointer(), count: op == "double-click" ? 2 : 1, right: op == "right-click")
    case "key":
      waitForIdle()
      requireFrontmost(app)
      key(args.first as? String ?? "")
    case "type":
      waitForIdle()
      requireFrontmost(app)
      type(args.first as? String ?? "")
    case "press":
      let target = Target(args.first, defaultApp: app)
      _ = resolve(target)
      guard let element = target.element() else { fail("could not find \(target.description)") }
      AXUIElementPerformAction(element, kAXPressAction as CFString)
    case "wait-for":
      let target = Target(args.first, defaultApp: app)
      _ = resolve(target, timeout: (args.dropFirst().first as? NSNumber)?.doubleValue ?? 60)
    case "wait-gone":
      let target = Target(args.first, defaultApp: app)
      let deadline = Date().addingTimeInterval((args.dropFirst().first as? NSNumber)?.doubleValue ?? 60)
      while target.element() != nil {
        if Date() > deadline { fail("\(target.description) is still there") }
        Thread.sleep(forTimeInterval: 0.2)
      }
    case "wait-file":
      let path = args.first as? String ?? ""
      let deadline = Date().addingTimeInterval((args.dropFirst().first as? NSNumber)?.doubleValue ?? 60)
      while !FileManager.default.fileExists(atPath: path) {
        if Date() > deadline { fail("timed out waiting for \(path)") }
        waitForIdle()
        Thread.sleep(forTimeInterval: 0.1)
      }
    case "window":
      // The app's window: [w, h] centred on its screen, or [x, y, w, h] in global points.
      let numbers = (args.first as? [NSNumber] ?? []).map(\.doubleValue)
      guard let current = mainWindowFrame(app) else { fail("\(app) has no window") }
      var rect = CGRect(x: current.minX, y: current.minY, width: numbers.first ?? current.width, height: numbers.dropFirst().first ?? current.height)
      if numbers.count == 4 {
        rect = CGRect(x: numbers[0], y: numbers[1], width: numbers[2], height: numbers[3])
      } else if let screen = NSScreen.screens.first {
        // Global points have their origin at the top left of the main screen.
        let visible = screen.visibleFrame, full = screen.frame
        let top = full.maxY - visible.maxY
        rect.origin = CGPoint(x: visible.minX + (visible.width - rect.width) / 2, y: top + (visible.height - rect.height) / 2)
      }
      setFrame(of: largestWindow(app), rect)
    case "window-of":
      // Another app's front window, at [x, y, w, h] relative to the app's window.
      let other = args.first as? String ?? ""
      let numbers = (args.dropFirst().first as? [NSNumber] ?? []).map(\.doubleValue)
      guard numbers.count == 4, let anchor = mainWindowFrame(app), let window = windows(of: other).first else { fail("window-of needs an app with a window and [x, y, w, h]") }
      setFrame(of: window, CGRect(x: anchor.minX + numbers[0], y: anchor.minY + numbers[1], width: numbers[2], height: numbers[3]))
    default:
      fail("unknown step \(op)")
    }
  }
}

func dump(_ bundleID: String) {
  func walk(_ element: AXUIElement, _ depth: Int) {
    guard depth < 40 else { return }
    for child in children(element) {
      let role = string(child, kAXRoleAttribute)
      let names = [kAXTitleAttribute, kAXDescriptionAttribute, kAXIdentifierAttribute, kAXValueAttribute].map { string(child, $0) }
      if names.contains(where: { !$0.isEmpty }), let f = frame(child) {
        let text = zip(["title", "desc", "id", "value"], names).filter { !$0.1.isEmpty }.map { "\($0.0)=\($0.1.prefix(60))" }.joined(separator: " ")
        print(String(repeating: "  ", count: min(depth, 12)) + "\(role) \(text) @\(Int(f.minX)),\(Int(f.minY)) \(Int(f.width))x\(Int(f.height))")
      }
      walk(child, depth + 1)
    }
  }
  for window in windows(of: bundleID) {
    print("window \(string(window, kAXTitleAttribute)) \(frame(window).map { "\($0)" } ?? "")")
    walk(window, 1)
  }
}

// MARK: Main

var arguments = CommandLine.arguments.dropFirst().makeIterator()
var storyboard = ""
var out = ""
var section = "steps"
while let argument = arguments.next() {
  switch argument {
  case "--dump":
    guard AXIsProcessTrusted() else { fail("this terminal needs Accessibility permission") }
    dump(arguments.next() ?? "")
    exit(0)
  case "--storyboard": storyboard = arguments.next() ?? ""
  case "--out": out = arguments.next() ?? ""
  case "--steps": section = arguments.next() ?? "steps"
  case "--idle": minimumIdle = Double(arguments.next() ?? "") ?? minimumIdle
  case "--set":
    // --set name=value: "{name}" in the steps becomes value.
    let pair = (arguments.next() ?? "").split(separator: "=", maxSplits: 1).map(String.init)
    if pair.count == 2 { placeholders[pair[0]] = pair[1] }
  case "--bookmark":
    // A plain bookmark to a folder, as hex for a `-downloadFolderBookmark <hex>` launch argument.
    let url = URL(fileURLWithPath: arguments.next() ?? "", isDirectory: true)
    guard let data = try? url.bookmarkData() else { fail("could not bookmark \(url.path)") }
    print("<" + data.map { String(format: "%02x", $0) }.joined() + ">")
    exit(0)
  default: fail("unknown argument \(argument)")
  }
}
guard !storyboard.isEmpty, !out.isEmpty else { fail("usage: drive-mac --storyboard FILE --out TAKE_DIR [--steps setup] | --dump <bundle id>") }
guard AXIsProcessTrusted() else { fail("this terminal needs Accessibility permission") }
guard let data = FileManager.default.contents(atPath: storyboard),
  let board = try? JSONSerialization.jsonObject(with: data) as? [String: Any], let take = board["take"] as? [String: Any],
  let app = take["app"] as? String
else { fail("\(storyboard) has no take.app") }
let steps = take[section] as? [[Any]] ?? []
placeholders["take"] = placeholders["take"] ?? URL(fileURLWithPath: out).standardizedFileURL.path
let marksURL = URL(fileURLWithPath: out).appendingPathComponent("marks.json")
var marks = (try? JSONSerialization.jsonObject(with: Data(contentsOf: marksURL)) as? [String: Double]) ?? [:]
run(steps: steps, app: app, marks: &marks, marksURL: marksURL)
saveMarks(marks, to: marksURL)
