#!/usr/bin/env swift
//
// Draws the dl-nzb app icon and menu bar glyph, and writes them where the app
// targets pick them up:
//
//   swift apple/Resources/make-icon.swift            # AppIcon.icon + Shared.xcassets
//   swift apple/Resources/make-icon.swift --preview  # also IconConcepts/preview.png
//
// The outputs are committed, so this only runs when the artwork changes.
//
// The picture is a download arrow assembled from parts: three segment bars
// for the shaft, then a solid head, the way an NZB's articles arrive one by one
// and become a file. The segments run yellow, green and cyan into a cyan head,
// on #2d2d2d: base16-eighties, the colours of the CLI and the site. Flat, no
// gradients; the system supplies the glass.
//
// The icon is an Icon Composer document (AppIcon.icon): icon.json plus one
// SVG per layer on Apple's 1024 pt canvas, where the rounded square fills the
// whole canvas (actool adds the macOS margin and shadow when it builds the
// .icns). Coordinates below are top-left origin, y down, like the canvas.
//
// The menu bar glyph is drawn per pixel at 1x and 2x from the same proportions,
// because a 16 pt template image only stays crisp if its edges land on pixels.
//
// --preview needs Xcode 26: it compiles the icon with actool and renders the
// appearance variants with Icon Composer's ictool.

import AppKit
import CoreGraphics
import Foundation

let arguments = CommandLine.arguments
let wantsPreview = arguments.contains("--preview")

let scriptURL = URL(fileURLWithPath: arguments[0]).resolvingSymlinksInPath()
let resources = scriptURL.deletingLastPathComponent()
let iconDocument = resources.appendingPathComponent("AppIcon.icon")
let catalog = resources.appendingPathComponent("Shared.xcassets")
let conceptsDirectory = resources.appendingPathComponent("IconConcepts")
let fileManager = FileManager.default

func fail(_ message: String) -> Never {
  FileHandle.standardError.write(Data("make-icon: \(message)\n".utf8))
  exit(1)
}

func write(_ text: String, to url: URL) {
  do {
    try fileManager.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    try text.write(to: url, atomically: true, encoding: .utf8)
  } catch {
    fail("could not write \(url.path): \(error)")
  }
}

// Fixed-precision numbers so the generated files only change when the art does.
func number(_ value: CGFloat) -> String {
  let text = String(format: "%.2f", Double(value))
  return text.hasSuffix(".00") ? String(text.dropLast(3)) : text
}

// MARK: - Palette

struct RGB {
  let hex: UInt32
  var red: CGFloat { CGFloat((hex >> 16) & 0xFF) / 255 }
  var green: CGFloat { CGFloat((hex >> 8) & 0xFF) / 255 }
  var blue: CGFloat { CGFloat(hex & 0xFF) / 255 }
  var svg: String { String(format: "#%06X", hex) }
  // Icon Composer's colour notation.
  var iconComposer: String { String(format: "srgb:%.5f,%.5f,%.5f,1.00000", red, green, blue) }
  var cgColor: CGColor { CGColor(srgbRed: red, green: green, blue: blue, alpha: 1) }
}

let ground = RGB(hex: 0x2D2D2D)      // base16-eighties base00
let groundDark = RGB(hex: 0x1F1F1F)  // a step darker for the Dark appearance
let yellow = RGB(hex: 0xFFCC66)      // base16-eighties base0A
let green = RGB(hex: 0x99CC99)       // base0B
let cyan = RGB(hex: 0x66CCCC)        // base0C
let white = RGB(hex: 0xFFFFFF)

// MARK: - Geometry (1024 pt canvas, y down)

let canvas: CGFloat = 1024
let centreX = canvas / 2

// The arrow is 630 pt tall. It sits 8 pt above the canvas centre because the
// head carries most of the weight.
let shaftWidth: CGFloat = 224
let segmentHeight: CGFloat = 88
let segmentGap: CGFloat = 30
let segmentRadius: CGFloat = 20
let headHalfWidth: CGFloat = 276        // 45 degree sides, so this is also the head's height
let shoulderRadius: CGFloat = 22
let tipRadius: CGFloat = 34
let arrowTop: CGFloat = 189

// Top to bottom, so the last segment matches the head.
let segmentColors = [yellow, green, cyan]

let segments: [CGRect] = (0..<3).map { index in
  CGRect(x: centreX - shaftWidth / 2, y: arrowTop + CGFloat(index) * (segmentHeight + segmentGap),
         width: shaftWidth, height: segmentHeight)
}
let headTop = arrowTop + 3 * (segmentHeight + segmentGap)
let head = [CGPoint(x: centreX - headHalfWidth, y: headTop),
            CGPoint(x: centreX + headHalfWidth, y: headTop),
            CGPoint(x: centreX, y: headTop + headHalfWidth)]
let headRadii = [shoulderRadius, shoulderRadius, tipRadius]

// A polygon with each corner replaced by a circular arc of the given radius,
// as SVG path data. Points go clockwise on screen, so every arc sweeps
// clockwise (SVG sweep flag 1).
func roundedPolygonPathData(_ points: [CGPoint], radii: [CGFloat]) -> String {
  func unit(_ from: CGPoint, _ to: CGPoint) -> CGVector {
    let dx = to.x - from.x, dy = to.y - from.y, length = (dx * dx + dy * dy).squareRoot()
    return CGVector(dx: dx / length, dy: dy / length)
  }
  var corners: [(enter: CGPoint, exit: CGPoint, radius: CGFloat)] = []
  for index in points.indices {
    let point = points[index]
    let previous = points[(index + points.count - 1) % points.count]
    let next = points[(index + 1) % points.count]
    let back = unit(point, previous), forward = unit(point, next)
    let angle = acos(back.dx * forward.dx + back.dy * forward.dy)
    let distance = radii[index] / tan(angle / 2)
    corners.append((CGPoint(x: point.x + back.dx * distance, y: point.y + back.dy * distance),
                    CGPoint(x: point.x + forward.dx * distance, y: point.y + forward.dy * distance),
                    radii[index]))
  }
  var data = "M\(number(corners[0].enter.x)) \(number(corners[0].enter.y))"
  for (index, corner) in corners.enumerated() {
    if index > 0 { data += " L\(number(corner.enter.x)) \(number(corner.enter.y))" }
    data += " A\(number(corner.radius)) \(number(corner.radius)) 0 0 1 \(number(corner.exit.x)) \(number(corner.exit.y))"
  }
  return data + " Z"
}

func svgDocument(_ body: String) -> String {
  """
  <svg xmlns="http://www.w3.org/2000/svg" width="\(number(canvas))" height="\(number(canvas))" \
  viewBox="0 0 \(number(canvas)) \(number(canvas))">
  \(body)
  </svg>

  """
}

// MARK: - AppIcon.icon

// One layer per piece in one group, so each catches the glass light on its own
// edges: the segments read as separate pieces even when the system draws the
// icon as clear or tinted glass. Each segment needs its own layer for its own
// colour too, because a layer's fill replaces the colours in its SVG.
let segmentSVGs = zip(segments, segmentColors).map { rect, color in
  svgDocument("  <rect x=\"\(number(rect.minX))\" y=\"\(number(rect.minY))\" width=\"\(number(rect.width))\" " +
    "height=\"\(number(rect.height))\" rx=\"\(number(segmentRadius))\" fill=\"\(color.svg)\"/>")
}
let headSVG = svgDocument("  <path d=\"\(roundedPolygonPathData(head, radii: headRadii))\" fill=\"\(cyan.svg)\"/>")

// The mark is white in the tinted appearance so the system's tint lands at
// full strength; in clear and tinted the system also replaces the ground.
// Specular highlights are off: the glass keeps its soft edge and depth without
// the bright rim, which on a flat mark reads as plastic.
func layer(_ name: String, _ color: RGB) -> [String: Any] {
  [
    "name": name,
    "image-name": "\(name).svg",
    "glass": true,
    "fill-specializations": [
      ["value": ["solid": color.iconComposer]],
      ["appearance": "tinted", "value": ["solid": white.iconComposer]],
    ],
    "position": ["scale": 1, "translation-in-points": [0, 0]],
  ]
}

// NSDecimalNumber keeps fractions like 0.5 short in the JSON.
func decimal(_ text: String) -> NSDecimalNumber { NSDecimalNumber(string: text) }

let iconJSON: [String: Any] = [
  "fill-specializations": [
    ["value": ["solid": ground.iconComposer]],
    ["appearance": "dark", "value": ["solid": groundDark.iconComposer]],
  ],
  "groups": [
    [
      "name": "Arrow",
      "layers": [layer("head", cyan)] + segmentColors.indices.map { layer("segment-\($0 + 1)", segmentColors[$0]) },
      "lighting": "individual",
      "specular": false,
      "shadow": ["kind": "neutral", "opacity": decimal("0.5")] as [String: Any],
      "translucency": ["enabled": true, "value": decimal("0.2")] as [String: Any],
    ] as [String: Any],
  ],
  "supported-platforms": ["squares": "shared"],
]

do {
  try? fileManager.removeItem(at: iconDocument)
  let assets = iconDocument.appendingPathComponent("Assets")
  for (index, svg) in segmentSVGs.enumerated() {
    write(svg, to: assets.appendingPathComponent("segment-\(index + 1).svg"))
  }
  write(headSVG, to: assets.appendingPathComponent("head.svg"))
  let json = try JSONSerialization.data(withJSONObject: iconJSON, options: [.prettyPrinted, .sortedKeys])
  write(String(decoding: json, as: UTF8.self) + "\n", to: iconDocument.appendingPathComponent("icon.json"))
  print("wrote \(iconDocument.path)")
} catch {
  fail("could not encode icon.json: \(error)")
}

// MARK: - Menu bar glyph

// Pixel geometry for a 16 pt template image, at 1x and 2x. Same proportions as
// the icon (shaft about 0.4 of the head's width, 45 degree head, three
// segments), snapped so every horizontal edge sits on a pixel boundary.
// MenuBarIconActive drops the arrow onto a bar, the file it is filling.
struct Glyph {
  var shaft: ClosedRange<CGFloat>          // x extent of the segments
  var segments: [ClosedRange<CGFloat>]     // y extent of each segment
  var headTop: CGFloat
  var headHalfWidth: CGFloat
  var tray: CGRect?
  var radius: CGFloat
}

func glyph(scale: Int, active: Bool) -> Glyph {
  switch (scale, active) {
  case (1, false):
    return Glyph(shaft: 6...10, segments: [1...3, 4...6, 7...9], headTop: 10, headHalfWidth: 5, tray: nil, radius: 0)
  case (1, true):
    return Glyph(shaft: 6...10, segments: [0...2, 3...5, 6...8], headTop: 9, headHalfWidth: 4,
                 tray: CGRect(x: 2, y: 14, width: 12, height: 1), radius: 0)
  case (_, false):
    return Glyph(shaft: 11...21, segments: [1...5, 7...11, 13...17], headTop: 19, headHalfWidth: 12, tray: nil, radius: 1)
  case (_, true):
    return Glyph(shaft: 12...20, segments: [1...4, 6...9, 11...14], headTop: 16, headHalfWidth: 10,
                 tray: CGRect(x: 4, y: 29, width: 24, height: 2), radius: 1)
  }
}

let sRGB = CGColorSpace(name: CGColorSpace.sRGB)!

// A bitmap context flipped to y-down.
func makeContext(width: Int, height: Int) -> CGContext {
  guard let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                space: sRGB, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
    fail("could not create a \(width)x\(height) bitmap")
  }
  context.interpolationQuality = .high
  context.translateBy(x: 0, y: CGFloat(height))
  context.scaleBy(x: 1, y: -1)
  return context
}

func drawGlyph(_ glyph: Glyph, in context: CGContext, color: CGColor) {
  context.setFillColor(color)
  for rows in glyph.segments {
    let rect = CGRect(x: glyph.shaft.lowerBound, y: rows.lowerBound,
                      width: glyph.shaft.upperBound - glyph.shaft.lowerBound, height: rows.upperBound - rows.lowerBound)
    context.addPath(CGPath(roundedRect: rect, cornerWidth: glyph.radius, cornerHeight: glyph.radius, transform: nil))
  }
  let middle = (glyph.shaft.lowerBound + glyph.shaft.upperBound) / 2
  context.move(to: CGPoint(x: middle - glyph.headHalfWidth, y: glyph.headTop))
  context.addLine(to: CGPoint(x: middle + glyph.headHalfWidth, y: glyph.headTop))
  context.addLine(to: CGPoint(x: middle, y: glyph.headTop + glyph.headHalfWidth))
  context.closePath()
  if let tray = glyph.tray {
    context.addPath(CGPath(roundedRect: tray, cornerWidth: min(glyph.radius, tray.height / 2),
                           cornerHeight: min(glyph.radius, tray.height / 2), transform: nil))
  }
  context.fillPath()
}

func pngData(_ image: CGImage) -> Data {
  let representation = NSBitmapImageRep(cgImage: image)
  guard let data = representation.representation(using: .png, properties: [:]) else { fail("could not encode a PNG") }
  return data
}

func glyphImage(scale: Int, active: Bool, color: CGColor = CGColor(gray: 0, alpha: 1)) -> CGImage {
  let pixels = 16 * scale
  let context = makeContext(width: pixels, height: pixels)
  drawGlyph(glyph(scale: scale, active: active), in: context, color: color)
  guard let image = context.makeImage() else { fail("could not render the menu bar glyph") }
  return image
}

// Only the two image sets are replaced; anything else in the catalog is left alone.
// No AccentColor: the apps use the system accent.
let catalogInfo = "  \"info\" : {\n    \"author\" : \"xcode\",\n    \"version\" : 1\n  }"
let catalogContents = catalog.appendingPathComponent("Contents.json")
if !fileManager.fileExists(atPath: catalogContents.path) {
  write("{\n\(catalogInfo)\n}\n", to: catalogContents)
}
for (name, active) in [("MenuBarIcon", false), ("MenuBarIconActive", true)] {
  let set = catalog.appendingPathComponent("\(name).imageset")
  try? fileManager.removeItem(at: set)
  var images: [String] = []
  for scale in [1, 2] {
    let filename = scale == 1 ? "\(name).png" : "\(name)@\(scale)x.png"
    do {
      try fileManager.createDirectory(at: set, withIntermediateDirectories: true)
      try pngData(glyphImage(scale: scale, active: active)).write(to: set.appendingPathComponent(filename))
    } catch {
      fail("could not write \(filename): \(error)")
    }
    images.append("    {\n      \"filename\" : \"\(filename)\",\n      \"idiom\" : \"universal\",\n      \"scale\" : \"\(scale)x\"\n    }")
  }
  write("""
    {
      "images" : [
    \(images.joined(separator: ",\n"))
      ],
    \(catalogInfo),
      "properties" : {
        "template-rendering-intent" : "template"
      }
    }

    """, to: set.appendingPathComponent("Contents.json"))
}
print("wrote \(catalog.path)")

guard wantsPreview else { exit(0) }

// MARK: - Preview sheet

// Everything below only builds IconConcepts/preview.png for review.

func run(_ executable: String, _ arguments: [String]) {
  let process = Process()
  process.executableURL = URL(fileURLWithPath: executable)
  process.arguments = arguments
  process.standardOutput = FileHandle.nullDevice
  let errors = Pipe()
  process.standardError = errors
  do { try process.run() } catch { fail("could not run \(executable): \(error)") }
  process.waitUntilExit()
  guard process.terminationStatus == 0 else {
    let message = String(decoding: errors.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
    fail("\(URL(fileURLWithPath: executable).lastPathComponent) failed: \(message)")
  }
}

func loadImage(_ url: URL) -> CGImage {
  guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
        let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else { fail("could not read \(url.path)") }
  return image
}

let work = fileManager.temporaryDirectory.appendingPathComponent("dl-nzb-icon-\(getpid())")
try? fileManager.removeItem(at: work)
try? fileManager.createDirectory(at: work, withIntermediateDirectories: true)
defer { try? fileManager.removeItem(at: work) }

// The real macOS output: compile with actool into a stub app bundle and ask
// NSWorkspace for its icon, which is drawn the way Finder and the Dock draw it.
let stubApp = work.appendingPathComponent("Preview.app")
let stubResources = stubApp.appendingPathComponent("Contents/Resources")
let stubExecutable = stubApp.appendingPathComponent("Contents/MacOS/Preview")
do {
  try fileManager.createDirectory(at: stubResources, withIntermediateDirectories: true)
  try fileManager.createDirectory(at: stubExecutable.deletingLastPathComponent(), withIntermediateDirectories: true)
  try "#!/bin/sh\n".write(to: stubExecutable, atomically: true, encoding: .utf8)
  try fileManager.setAttributes([.posixPermissions: 0o755], ofItemAtPath: stubExecutable.path)
  // A fresh identifier each run so Launch Services does not hand back a cached icon.
  let info: [String: Any] = [
    "CFBundleIdentifier": "dev.dl-nzb.icon-preview.\(getpid()).\(Int(Date().timeIntervalSince1970))",
    "CFBundleExecutable": "Preview", "CFBundlePackageType": "APPL",
    "CFBundleIconFile": "AppIcon", "CFBundleIconName": "AppIcon",
  ]
  let plist = try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0)
  try plist.write(to: stubApp.appendingPathComponent("Contents/Info.plist"))
} catch {
  fail("could not build the preview bundle: \(error)")
}
run("/usr/bin/xcrun", ["actool", "--compile", stubResources.path, "--platform", "macosx", "--minimum-deployment-target", "26.0",
                       "--app-icon", "AppIcon", "--output-partial-info-plist", work.appendingPathComponent("partial.plist").path,
                       iconDocument.path])
let systemIcon = NSWorkspace.shared.icon(forFile: stubApp.path)

func render(_ image: NSImage, pixels: Int) -> CGImage {
  guard let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8,
                                      samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                                      bytesPerRow: 0, bitsPerPixel: 0) else { fail("could not make a bitmap") }
  bitmap.size = NSSize(width: pixels, height: pixels)
  NSGraphicsContext.saveGraphicsState()
  NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
  image.draw(in: NSRect(x: 0, y: 0, width: pixels, height: pixels))
  NSGraphicsContext.restoreGraphicsState()
  guard let image = bitmap.cgImage else { fail("could not render the system icon") }
  return image
}

// The appearance variants, from Icon Composer's renderer (iOS shapes). The
// tinted ones use the tint from Apple's ictool example.
let ictool = "/Applications/Xcode.app/Contents/Applications/Icon Composer.app/Contents/Executables/ictool"
let renditions = ["Default", "Dark", "ClearLight", "ClearDark", "TintedLight", "TintedDark"]
var variants: [String: CGImage] = [:]
for rendition in renditions {
  let output = work.appendingPathComponent("\(rendition).png")
  var options = [iconDocument.path, "--export-image", "--output-file", output.path, "--platform", "iOS",
                 "--rendition", rendition, "--width", "180", "--height", "180", "--scale", "1"]
  if rendition.hasPrefix("Tinted") { options += ["--tint-color", "0.25", "--tint-strength", "0.75"] }
  run(ictool, options)
  variants[rendition] = loadImage(output)
}

let sheetWidth = 2000, sheetHeight = 1380
let sheet = makeContext(width: sheetWidth, height: sheetHeight)
let paper = RGB(hex: 0xF2F0EC).cgColor, night = RGB(hex: 0x1E1E1E).cgColor
let ink = RGB(hex: 0x515151).cgColor, faint = RGB(hex: 0xA09F93).cgColor

sheet.setFillColor(paper)
sheet.fill(CGRect(x: 0, y: 0, width: sheetWidth, height: sheetHeight))

// Draws an image with its top-left at (x, y), optionally magnified without smoothing.
func place(_ image: CGImage, x: CGFloat, y: CGFloat, size: CGFloat? = nil, nearest: Bool = false) {
  let width = size ?? CGFloat(image.width), height = size ?? CGFloat(image.height)
  sheet.saveGState()
  sheet.interpolationQuality = nearest ? .none : .high
  sheet.translateBy(x: x, y: y + height)
  sheet.scaleBy(x: 1, y: -1)
  sheet.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
  sheet.restoreGState()
}

func label(_ text: String, x: CGFloat, y: CGFloat, color: CGColor = faint, size: CGFloat = 16) {
  let attributes: [NSAttributedString.Key: Any] = [
    .font: NSFont.monospacedSystemFont(ofSize: size, weight: .regular),
    .foregroundColor: NSColor(cgColor: color) ?? .gray,
  ]
  NSGraphicsContext.saveGraphicsState()
  NSGraphicsContext.current = NSGraphicsContext(cgContext: sheet, flipped: true)
  NSAttributedString(string: text, attributes: attributes).draw(at: CGPoint(x: x, y: y))
  NSGraphicsContext.restoreGraphicsState()
}

// 1024 as the system draws it, margin and shadow included.
place(render(systemIcon, pixels: 1024), x: 20, y: 10)
label("1024 · macOS, as Finder and the Dock draw it", x: 40, y: 1036, color: ink)

// 128, 32 and 16 at their real pixel sizes, then 32 and 16 magnified, on light and dark.
for (index, background) in [paper, night].enumerated() {
  let left = 1070 + CGFloat(index) * 460, top: CGFloat = 20
  sheet.setFillColor(background)
  sheet.fill(CGRect(x: left, y: top, width: 450, height: 450))
  place(render(systemIcon, pixels: 128), x: left + 20, y: top + 20)
  place(render(systemIcon, pixels: 32), x: left + 180, y: top + 68)
  place(render(systemIcon, pixels: 16), x: left + 240, y: top + 76)
  label("128 · 32 · 16", x: left + 20, y: top + 160)
  place(render(systemIcon, pixels: 32), x: left + 20, y: top + 200, size: 192, nearest: true)
  place(render(systemIcon, pixels: 16), x: left + 232, y: top + 200, size: 192, nearest: true)
  label("32 ×6", x: left + 20, y: top + 400)
  label("16 ×12", x: left + 232, y: top + 400)
}

// The menu bar glyphs on a light and a dark bar: a 2x bar, a 1x bar, and the
// 2x pixels magnified.
for (index, (bar, tint)) in [(RGB(hex: 0xF6F5F2).cgColor, CGColor(gray: 0, alpha: 0.85)),
                             (RGB(hex: 0x2A2A2A).cgColor, CGColor(gray: 1, alpha: 0.9))].enumerated() {
  let left = 1070 + CGFloat(index) * 460, top: CGFloat = 490
  sheet.setFillColor(index == 0 ? paper : night)
  sheet.fill(CGRect(x: left, y: top, width: 450, height: 560))
  sheet.setFillColor(bar)
  sheet.fill(CGRect(x: left, y: top + 20, width: 450, height: 44))
  sheet.fill(CGRect(x: left, y: top + 84, width: 450, height: 22))
  for (column, active) in [false, true].enumerated() {
    let x = left + 30 + CGFloat(column) * 60
    place(glyphImage(scale: 2, active: active, color: tint), x: x, y: top + 26)
    place(glyphImage(scale: 1, active: active, color: tint), x: x + 8, y: top + 87)
    place(glyphImage(scale: 2, active: active, color: tint), x: left + 20 + CGFloat(column) * 210, y: top + 140,
          size: 192, nearest: true)
  }
  label("menu bar 2x and 1x", x: left + 170, y: top + 33)
  label("MenuBarIcon", x: left + 20, y: top + 344)
  label("MenuBarIconActive", x: left + 230, y: top + 344)
  label("@2x ×6", x: left + 20, y: top + 370)
}

// Appearance variants along the bottom.
for (index, rendition) in renditions.enumerated() {
  let x = 20 + CGFloat(index) * 330, top: CGFloat = 1080
  sheet.setFillColor(rendition.hasSuffix("Dark") ? night : RGB(hex: 0xDAD8D2).cgColor)
  sheet.fill(CGRect(x: x, y: top, width: 320, height: 280))
  place(variants[rendition]!, x: x + 70, y: top + 20)
  label("iOS · \(rendition)", x: x + 70, y: top + 230)
}

guard let sheetImage = sheet.makeImage() else { fail("could not render the preview") }
let previewURL = conceptsDirectory.appendingPathComponent("preview.png")
do {
  try fileManager.createDirectory(at: conceptsDirectory, withIntermediateDirectories: true)
  try pngData(sheetImage).write(to: previewURL)
} catch {
  fail("could not write \(previewURL.path): \(error)")
}
print("wrote \(previewURL.path)")
