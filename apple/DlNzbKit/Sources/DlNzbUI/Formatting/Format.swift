import Foundation

/// Sizes, speeds and times the way both apps show them: Finder's decimal
/// units, Apple's abbreviated durations, and numbers that do not flicker at
/// four updates a second.
///
/// Every function takes a locale so the tests can pin one; the apps use the
/// default, the user's.
public enum Format {
  /// "8.2 GB", "459 MB", "512 bytes": Finder's decimal units, with one rule
  /// for precision whatever the unit, so sizes side by side read alike: one
  /// decimal below 10 ("6.1 GB", "8.4 kB"), none from 10 up ("459 MB",
  /// "960 MB", "12 GB"). A trailing ".0" is dropped, as Finder does.
  public static func bytes(_ bytes: Int64, locale: Locale = .autoupdatingCurrent) -> String {
    self.bytes(bytes, keepsTrailingZero: false, locale: locale)
  }

  /// `keepsTrailingZero`: "2.0 GB" rather than "2 GB", for a count that
  /// climbs past it ("1.9 GB of 3.3 GB", "2.0 GB of 3.3 GB").
  private static func bytes(_ bytes: Int64, keepsTrailingZero: Bool, locale: Locale) -> String {
    let value = Double(max(bytes, 0))
    guard value >= 1_000 else {
      let count = Int64(value)
      return "\(count.formatted(.number.locale(locale))) \(count == 1 ? "byte" : "bytes")"
    }
    let units = ["kB", "MB", "GB", "TB", "PB"]
    var scaled = value / 1_000
    var unit = 0
    while unit < units.count - 1, scaled.rounded() >= 1_000 {
      scaled /= 1_000
      unit += 1
    }
    // One decimal unless that would show 10.0 or more.
    let decimals = (scaled * 10).rounded() / 10 < 10 ? 1 : 0
    let fraction = keepsTrailingZero ? decimals...decimals : 0...decimals
    let number = scaled.formatted(.number.precision(.fractionLength(fraction)).grouping(.never).locale(locale))
    return "\(number) \(units[unit])"
  }

  /// "3.1 GB of 8.2 GB", "2.0 GB of 3.3 GB": both keep their decimal, so
  /// the line does not change length as the count passes a whole number.
  public static func bytes(_ done: Int64, of total: Int64, locale: Locale = .autoupdatingCurrent) -> String {
    "\(bytes(done, keepsTrailingZero: true, locale: locale)) of \(bytes(total, keepsTrailingZero: true, locale: locale))"
  }

  /// "84 MB/s". Rounded to whole units from 10 up and to a tenth below, so a
  /// speed that wobbles by a few hundred kilobytes does not change every tick.
  public static func speed(_ bytesPerSecond: Double, locale: Locale = .autoupdatingCurrent) -> String {
    let value = bytesPerSecond.isFinite ? max(bytesPerSecond, 0) : 0
    let rounded: Double =
      switch value {
      case 10_000_000_000...: (value / 1_000_000_000).rounded() * 1_000_000_000
      case 1_000_000_000...: (value / 100_000_000).rounded() * 100_000_000
      case 10_000_000...: (value / 1_000_000).rounded() * 1_000_000
      case 1_000_000...: (value / 100_000).rounded() * 100_000
      case 1_000...: (value / 1_000).rounded() * 1_000
      default: value.rounded()
      }
    return "\(bytes(Int64(rounded), locale: locale))/s"
  }

  /// "45 sec", "3 min", "1 hr, 5 min", "2 days, 3 hr": one unit under an
  /// hour (rounded to the nearest), two above.
  public static func duration(_ seconds: Double, locale: Locale = .autoupdatingCurrent) -> String {
    let total = seconds.isFinite ? max(Int64(seconds.rounded()), 1) : 1
    let style: Duration.UnitsFormatStyle
    let value: Int64
    switch total {
    case ..<60:
      value = total
      style = .units(allowed: [.seconds], width: .abbreviated)
    case ..<3_600:
      value = Int64((Double(total) / 60).rounded()) * 60
      style = .units(allowed: [.minutes], width: .abbreviated)
    case ..<86_400:
      value = Int64((Double(total) / 60).rounded()) * 60
      style = .units(allowed: [.hours, .minutes], width: .abbreviated, maximumUnitCount: 2)
    default:
      value = Int64((Double(total) / 3_600).rounded()) * 3_600
      style = .units(allowed: [.days, .hours], width: .abbreviated, maximumUnitCount: 2)
    }
    return Duration.seconds(value).formatted(style.locale(locale))
  }

  /// "1 min left".
  public static func timeLeft(_ seconds: Int64, locale: Locale = .autoupdatingCurrent) -> String {
    "\(duration(Double(seconds), locale: locale)) left"
  }

  /// "43%".
  public static func percent(_ fraction: Double, locale: Locale = .autoupdatingCurrent) -> String {
    let clamped = fraction.isFinite ? min(max(fraction, 0), 1) : 0
    return clamped.formatted(.percent.precision(.fractionLength(0)).locale(locale))
  }

  /// "9%", or "Less than 1%" for a share that would round to nothing.
  public static func share(_ fraction: Double, locale: Locale = .autoupdatingCurrent) -> String {
    fraction > 0 && fraction < 0.005 ? "Less than \(percent(0.01, locale: locale))" : percent(fraction, locale: locale)
  }

  /// "21,840".
  public static func count(_ value: Int, locale: Locale = .autoupdatingCurrent) -> String {
    value.formatted(.number.locale(locale))
  }

  /// "1 block", "12 blocks".
  public static func count(_ value: Int, _ singular: String, _ plural: String, locale: Locale = .autoupdatingCurrent) -> String {
    "\(count(value, locale: locale)) \(value == 1 ? singular : plural)"
  }

  /// "2 of 5", "46 of 11,090".
  public static func count(_ value: Int, of total: Int, locale: Locale = .autoupdatingCurrent) -> String {
    "\(count(value, locale: locale)) of \(count(total, locale: locale))"
  }
}
