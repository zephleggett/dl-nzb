import SwiftUI

/// How a state reads at a glance. Glyphs and bars take the system's semantic
/// colours, which follow Dark Mode, Increase Contrast and the accent colour.
/// Words take `textStyle`, which keeps 4.5:1 against the window: the system's
/// orange and green are glyph colours, too light to read as text on white.
public enum StatusTone: Equatable, Sendable {
  case neutral
  /// Running: the accent colour.
  case active
  case good
  case warning
  case bad

  /// The system colour, for glyphs, bars and other marks only. Text uses
  /// `textStyle` or `emphasisStyle`.
  public var color: Color {
    switch self {
    case .neutral: .secondary
    case .active: .accentColor
    case .good: .green
    case .warning: .orange
    case .bad: .red
    }
  }

  /// For a status line: neutral, running and finished lines are secondary;
  /// only problems are tinted, in a shade that stays readable.
  public var textStyle: ToneStyle {
    switch self {
    case .neutral, .active, .good: ToneStyle(text: nil)
    case .warning, .bad: ToneStyle(text: self)
    }
  }

  /// For words that carry the tone whatever it is: a headline such as
  /// "Password Required", or "Connected · 38 ms" beside Test Connection.
  /// Readable in every appearance; neutral and running are secondary.
  public var emphasisStyle: ToneStyle {
    switch self {
    case .neutral, .active: ToneStyle(text: nil)
    case .good, .warning, .bad: ToneStyle(text: self)
    }
  }

  /// For glyphs: always the tone's system colour.
  public var glyphStyle: ToneStyle {
    ToneStyle(self == .neutral ? nil : color)
  }

  /// The text colour in light mode at standard contrast, where the system
  /// colours fall short of 4.5:1 (orange about 2.2:1, green 2.2:1, red
  /// 3.6:1 on white). Darker shades of the same hues, each at least 4.5:1 on
  /// white and on the window and grouped-list greys. Nil where the tone has
  /// no text colour of its own. Dark Mode and Increase Contrast use the
  /// system colours, which pass there.
  public var lightTextRGB: (red: Double, green: Double, blue: Double)? {
    switch self {
    case .neutral, .active: nil
    case .good: (26 / 255, 118 / 255, 48 / 255)
    case .warning: (176 / 255, 58 / 255, 0)
    case .bad: (196 / 255, 0, 20 / 255)
    }
  }

  /// The colour words in this tone take in an environment; nil for the
  /// secondary style.
  public func textColor(colorScheme: ColorScheme, contrast: ColorSchemeContrast) -> Color? {
    guard let rgb = lightTextRGB else { return nil }
    if colorScheme == .light && contrast == .standard {
      return Color(.sRGB, red: rgb.red, green: rgb.green, blue: rgb.blue)
    }
    return color
  }
}

/// A tint that gives way to the selection's own text colour on a selected
/// row, where it would otherwise sit on the accent and vanish. Untinted, it
/// is the secondary style.
public struct ToneStyle: ShapeStyle {
  let color: Color?
  let textTone: StatusTone?

  /// A glyph's tint: `color` as it is.
  public init(_ color: Color?) {
    self.color = color
    self.textTone = nil
  }

  /// Words in `tone`'s readable shade (`StatusTone.textColor`).
  init(text tone: StatusTone?) {
    self.color = nil
    self.textTone = tone
  }

  public func resolve(in environment: EnvironmentValues) -> AnyShapeStyle {
    if environment.backgroundProminence == .increased { return AnyShapeStyle(.primary) }
    if let textTone {
      let color = textTone.textColor(colorScheme: environment.colorScheme, contrast: environment.colorSchemeContrast)
      return color.map { AnyShapeStyle($0) } ?? AnyShapeStyle(.secondary)
    }
    return color.map { AnyShapeStyle($0) } ?? AnyShapeStyle(.secondary)
  }
}
