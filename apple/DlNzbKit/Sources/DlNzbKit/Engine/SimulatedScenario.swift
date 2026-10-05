import Foundation

/// What a simulated job will do. Chosen from hints in the release name so a
/// preview, a test or a screenshot can ask for any state by naming its NZB:
///
/// | Name contains            | Scenario       | Ends as                                   |
/// |--------------------------|----------------|-------------------------------------------|
/// | `unrepairable`           | `.unrepairable`| Needs Attention after the pre-flight scan |
/// | `password`, `encrypted`  | `.password`    | Password Required after downloading       |
/// | `diskfull`               | `.diskFull`    | Not enough space, before downloading      |
/// | `badlogin`               | `.authFailure` | Server rejects the login                  |
/// | `offline`                | `.unreachable` | Server cannot be reached                  |
/// | `fail`                   | `.failure`     | Failed: too many articles missing         |
/// | `repair`                 | `.repair`      | Recovery data, 12 blocks repaired         |
/// | anything else            | `.normal` or `.repair`, from a stable hash of the name |
///
/// A password job succeeds with any password that does not contain "wrong".
public enum SimulatedScenario: String, Sendable, Codable, CaseIterable {
  case normal
  case repair
  case unrepairable
  case password
  case failure
  case diskFull
  case authFailure
  case unreachable

  public init(hint: String) {
    let lower = hint.lowercased()
    if lower.contains("unrepairable") {
      self = .unrepairable
    } else if lower.contains("password") || lower.contains("encrypted") {
      self = .password
    } else if lower.contains("diskfull") || lower.contains("disk-full") || lower.contains("disk.full") {
      self = .diskFull
    } else if lower.contains("badlogin") {
      self = .authFailure
    } else if lower.contains("offline") {
      self = .unreachable
    } else if lower.contains("fail") {
      self = .failure
    } else if lower.contains("repair") {
      self = .repair
    } else {
      // About one release in four needs its recovery data, as in life.
      // FNV-1a's low bits follow only the low bits of each byte, so take high ones.
      self = (StableHash.of(lower) >> 32) % 4 == 0 ? .repair : .normal
    }
  }

  /// Whether the job loses articles on the way and needs PAR2's help.
  var losesArticles: Bool {
    self == .repair || self == .failure || self == .unrepairable
  }

  /// Share of articles missing on the server.
  var missingShare: Double {
    switch self {
    case .repair: 0.004
    case .failure, .unrepairable: 0.09
    default: 0
    }
  }
}

/// FNV-1a: the same number for the same name on every run, unlike `hashValue`.
enum StableHash {
  static func of(_ string: String) -> UInt64 {
    var hash: UInt64 = 0xcbf2_9ce4_8422_2325
    for byte in string.utf8 {
      hash ^= UInt64(byte)
      hash = hash &* 0x0000_0100_0000_01b3
    }
    return hash
  }
}

/// SplitMix64: a small seeded generator, so a simulated job's speed wobbles
/// the same way every time it runs.
struct SeededGenerator: RandomNumberGenerator {
  private var state: UInt64

  init(seed: UInt64) {
    state = seed
  }

  mutating func next() -> UInt64 {
    state &+= 0x9E37_79B9_7F4A_7C15
    var z = state
    z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
    z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
    return z ^ (z >> 31)
  }
}
