import Foundation
import Observation

/// The settings only iPhone and iPad have, beside the Kit's `SettingsStore`.
@MainActor
@Observable
final class PhoneSettings {
  static let allowsCellularKey = "allowsCellular"

  /// Off: dl-nzb asks before downloading on cellular, a personal hotspot or
  /// Low Data Mode. A release is gigabytes; a surprise bill is worse than a
  /// question.
  var allowsCellular: Bool {
    didSet { defaults.set(allowsCellular, forKey: Self.allowsCellularKey) }
  }

  @ObservationIgnored private let defaults: UserDefaults

  init(defaults: UserDefaults = .standard) {
    self.defaults = defaults
    allowsCellular = defaults.bool(forKey: Self.allowsCellularKey)
  }
}
