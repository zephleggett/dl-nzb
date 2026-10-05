import DlNzbKit
import UniformTypeIdentifiers
import os

extension UTType {
  /// An NZB file. Declared in Info.plist as an imported type, so dl-nzb is
  /// offered for .nzb files without claiming to own the format.
  static let nzb = UTType(importedAs: "com.zephleggett.dl-nzb.nzb", conformingTo: .xml)
}

extension URL {
  /// Whether a dropped or opened file looks like an NZB. By extension, as
  /// Finder decides: the queue reads and checks the contents itself.
  var isNZB: Bool {
    pathExtension.lowercased() == "nzb"
  }
}

extension Log {
  /// The Mac app's own logger, beside the Kit's categories.
  static let mac = Logger(subsystem: Log.subsystem, category: "mac")
}
