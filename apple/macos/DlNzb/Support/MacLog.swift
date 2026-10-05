import DlNzbKit
import os

extension Log {
  /// The Mac app's own logger, beside the Kit's categories.
  static let mac = Logger(subsystem: Log.subsystem, category: "mac")
}
