import DlNzbKit
import os

/// The iPhone app's own log categories, beside the Kit's (`Log.app`,
/// `Log.queue` …) and under the same subsystem, so
/// `log stream --predicate 'subsystem == "com.zephleggett.dl-nzb"'` shows both.
enum AppLog {
  static let background = Logger(subsystem: Log.subsystem, category: "background")
  static let network = Logger(subsystem: Log.subsystem, category: "network")
  static let open = Logger(subsystem: Log.subsystem, category: "open")
  static let notifications = Logger(subsystem: Log.subsystem, category: "notifications")
}
