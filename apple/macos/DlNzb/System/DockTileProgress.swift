import AppKit
import DlNzbKit

/// What the Dock tile should be doing, as a small state machine the tests
/// can drive with their own clock.
///
/// The custom tile exists only while something downloads; the rest of the
/// time the Dock draws the icon itself, crisp at every size (the owner chose
/// this trade-off over a tile that is always custom). Redraws are throttled
/// to about twice a second: the engine reports four times a second per job,
/// and every `display()` re-renders the whole tile.
struct DockTileMachine: Equatable {
  enum Effect: Equatable {
    /// Put the custom view in the tile.
    case install
    /// Draw the bar at this fraction now.
    case redraw(Double)
    /// Hand the tile back to the Dock.
    case restore
    case badge(String?)
    /// Call `redrawDue(now:)` after this long.
    case scheduleRedraw(Duration)
  }

  var minimumInterval: Duration = .milliseconds(500)
  private(set) var isShowing = false
  private(set) var drawnFraction: Double?
  private(set) var latestFraction: Double?
  private(set) var badge: String?
  private(set) var isRedrawScheduled = false
  private var lastDraw: ContinuousClock.Instant?

  /// - Parameters:
  ///   - fraction: The queue's overall fraction, or nil when nothing runs.
  ///   - badgeCount: Unfinished downloads, shown while the tile is custom.
  mutating func update(fraction: Double?, badgeCount: Int, now: ContinuousClock.Instant) -> [Effect] {
    var effects: [Effect] = []
    guard let fraction else {
      if isShowing {
        effects.append(.restore)
        isShowing = false
        drawnFraction = nil
        latestFraction = nil
        lastDraw = nil
        isRedrawScheduled = false
      }
      if badge != nil {
        badge = nil
        effects.append(.badge(nil))
      }
      return effects
    }
    let value = Self.quantised(fraction)
    latestFraction = value
    if !isShowing {
      isShowing = true
      effects += [.install, .redraw(value)]
      drawnFraction = value
      lastDraw = now
    } else if value != drawnFraction, !isRedrawScheduled {
      if let lastDraw, now - lastDraw < minimumInterval {
        isRedrawScheduled = true
        effects.append(.scheduleRedraw(minimumInterval - (now - lastDraw)))
      } else {
        effects.append(.redraw(value))
        drawnFraction = value
        lastDraw = now
      }
    }
    let label = badgeCount > 0 ? badgeCount.formatted() : nil
    if label != badge {
      badge = label
      effects.append(.badge(label))
    }
    return effects
  }

  /// A scheduled redraw has come due: draw the latest fraction if it moved.
  mutating func redrawDue(now: ContinuousClock.Instant) -> [Effect] {
    guard isRedrawScheduled else { return [] }
    isRedrawScheduled = false
    guard isShowing, let latestFraction, latestFraction != drawnFraction else { return [] }
    drawnFraction = latestFraction
    lastDraw = now
    return [.redraw(latestFraction)]
  }

  /// To a two-hundredth: finer than the bar has pixels, so a change that
  /// would not show does not cost a redraw.
  static func quantised(_ fraction: Double) -> Double {
    let clamped = fraction.isFinite ? min(max(fraction, 0), 1) : 0
    return (clamped * 200).rounded() / 200
  }
}

/// The Dock icon with a progress bar along its bottom and a count of
/// unfinished downloads, only while something downloads.
@MainActor
final class DockTileProgress {
  private var machine = DockTileMachine()
  /// Looked up when needed: the services exist before NSApplication does.
  private var tile: NSDockTile { NSApp.dockTile }
  private lazy var view = DockProgressView(frame: NSRect(origin: .zero, size: tile.size))
  private var redrawTask: Task<Void, Never>?

  func update(fraction: Double?, unfinished: Int) {
    apply(machine.update(fraction: fraction, badgeCount: unfinished, now: .now))
  }

  /// Back to the plain icon, for quitting.
  func reset() {
    apply(machine.update(fraction: nil, badgeCount: 0, now: .now))
  }

  private func apply(_ effects: [DockTileMachine.Effect]) {
    for effect in effects {
      switch effect {
      case .install:
        view.frame = NSRect(origin: .zero, size: tile.size)
        tile.contentView = view
      case .redraw(let fraction):
        view.fraction = fraction
        tile.display()
      case .restore:
        redrawTask?.cancel()
        tile.contentView = nil
        tile.display()
      case .badge(let label):
        tile.badgeLabel = label
      case .scheduleRedraw(let delay):
        redrawTask?.cancel()
        redrawTask = Task { [weak self] in
          try? await Task.sleep(for: delay)
          guard !Task.isCancelled, let self else { return }
          self.apply(self.machine.redrawDue(now: .now))
        }
      }
    }
  }
}

/// The tile's view: the app icon as the Dock would draw it, and a bar near
/// the bottom of the icon's body in the system accent colour on a white track,
/// the shape Finder's copy progress has always had.
final class DockProgressView: NSView {
  var fraction: Double = 0

  override func draw(_ dirtyRect: NSRect) {
    NSApp.applicationIconImage?.draw(in: bounds)
    // The icon's body sits inside the canvas with a margin (about a tenth on
    // each side for a macOS 26 icon); the bar stays within it.
    let height = max((bounds.height * 0.085).rounded(), 6)
    let inset = (bounds.width * 0.2).rounded()
    let track = NSRect(x: inset, y: (bounds.height * 0.16).rounded(), width: bounds.width - inset * 2, height: height)
    let trackPath = NSBezierPath(roundedRect: track, xRadius: height / 2, yRadius: height / 2)

    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.35)
    shadow.shadowBlurRadius = 3
    shadow.shadowOffset = NSSize(width: 0, height: -1)
    shadow.set()
    NSColor.white.setFill()
    trackPath.fill()
    NSGraphicsContext.restoreGraphicsState()

    let inner = track.insetBy(dx: 1.5, dy: 1.5)
    let width = max(inner.height, inner.width * min(max(fraction, 0), 1))
    let fill = NSRect(x: inner.minX, y: inner.minY, width: width, height: inner.height)
    NSColor.controlAccentColor.setFill()
    NSBezierPath(roundedRect: fill, xRadius: inner.height / 2, yRadius: inner.height / 2).fill()
  }
}
