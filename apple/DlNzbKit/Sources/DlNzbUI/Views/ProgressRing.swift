import DlNzbKit
import SwiftUI

/// The App Store's download ring: a thin track, an arc for the fraction, and
/// a small glyph in the middle that says what tapping it does. The iPhone's
/// rows put it on their trailing edge (tap pauses or resumes).
///
/// Indeterminate (connecting, checking) is a short arc going round, or a
/// still dashed ring with Reduce Motion.
///
/// The ring and its glyph grow with Dynamic Type, as the text beside them does.
public struct ProgressRing: View {
  public enum Glyph: Sendable, Equatable {
    case pause
    case resume
    case stop
    /// Waiting for its turn: a clock, so the row does not read as running.
    case waiting
    case none

    var symbolName: String? {
      switch self {
      case .pause: "pause.fill"
      case .resume: "play.fill"
      case .stop: "stop.fill"
      case .waiting: "clock"
      case .none: nil
      }
    }
  }

  let fraction: Double?
  let glyph: Glyph
  let isPaused: Bool
  let lineWidth: CGFloat
  private let baseDiameter: CGFloat

  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  @ScaledMetric private var diameter: CGFloat
  @ScaledMetric private var glyphSize: CGFloat

  /// - Parameters:
  ///   - fraction: 0...1, or nil while the job has nothing to measure yet.
  ///   - isPaused: draws the arc in a secondary tint.
  ///   - diameter: the size at the default text size; it scales with Dynamic Type.
  public init(fraction: Double?, glyph: Glyph, isPaused: Bool = false, lineWidth: CGFloat = 2.5, diameter: CGFloat = 28) {
    self.fraction = fraction.map(\.clampedFraction)
    self.glyph = glyph
    self.isPaused = isPaused
    self.lineWidth = lineWidth
    self.baseDiameter = diameter
    _diameter = ScaledMetric(wrappedValue: diameter, relativeTo: .body)
    _glyphSize = ScaledMetric(wrappedValue: diameter * 9 / 28, relativeTo: .body)
  }

  /// The ring for an item: its fraction, and pause or resume as the tap action.
  public init(_ item: DownloadItem) {
    let progress = RowProgress.of(item)
    let fraction: Double? =
      switch progress {
      case .none: 0
      case .indeterminate: nil
      case .determinate(let value), .frozen(let value): value
      }
    let glyph: Glyph = item.canResume ? .resume : (item.canPause ? .pause : (item.canStop ? .stop : .none))
    self.init(fraction: fraction, glyph: glyph, isPaused: item.isPaused)
  }

  private var tint: Color { isPaused ? .secondary : .accentColor }

  /// The line thickens with the ring, so a large ring is not drawn in hairline.
  private var scaledLineWidth: CGFloat {
    baseDiameter > 0 ? lineWidth * diameter / baseDiameter : lineWidth
  }

  public var body: some View {
    let lineWidth = scaledLineWidth
    ZStack {
      Circle()
        .stroke(.quaternary, lineWidth: lineWidth)
      if let fraction {
        Circle()
          .trim(from: 0, to: max(fraction, 0.001))
          .stroke(tint, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
          .rotationEffect(.degrees(-90))
          .animation(.smooth(duration: 0.4), value: fraction)
      } else if reduceMotion {
        Circle()
          .stroke(tint, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round, dash: [2, 4]))
      } else {
        TimelineView(.animation) { context in
          let turns = context.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: 1)
          Circle()
            .trim(from: 0, to: 0.25)
            .stroke(tint, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
            .rotationEffect(.degrees(turns * 360))
        }
      }
      if let symbol = glyph.symbolName {
        Image(systemName: symbol)
          .font(.system(size: glyphSize, weight: .bold))
          .foregroundStyle(tint)
      }
    }
    .padding(lineWidth / 2)
    .frame(width: diameter, height: diameter)
    .contentShape(Circle())
    .accessibilityElement(children: .ignore)
    .accessibilityLabel(accessibilityText)
  }

  private var accessibilityText: String {
    let amount = fraction.map { Format.percent($0) } ?? "In progress"
    switch glyph {
    case .pause: return "\(amount). Pause"
    case .resume: return "\(amount). Resume"
    case .stop: return "\(amount). Stop"
    case .waiting: return "\(amount). Waiting"
    case .none: return amount
    }
  }
}

#Preview("Ring states") {
  HStack(spacing: 20) {
    ProgressRing(fraction: nil, glyph: .stop)
    ProgressRing(fraction: 0.38, glyph: .pause)
    ProgressRing(fraction: 0.41, glyph: .resume, isPaused: true)
    ProgressRing(fraction: 0.9, glyph: .stop)
    ProgressRing(fraction: 0, glyph: .waiting, isPaused: true)
    ProgressRing(PreviewData.downloading)
  }
  .padding()
}
