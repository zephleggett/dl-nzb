import Foundation
import UniformTypeIdentifiers

extension UTType {
  /// An NZB file. Both apps declare it in Info.plist as an imported type, so
  /// dl-nzb is offered for .nzb files without claiming to own the format.
  public static let nzb = UTType(importedAs: "com.zephleggett.dl-nzb.nzb", conformingTo: .xml)
}

extension URL {
  /// Whether a dropped or opened file looks like an NZB. By extension, as
  /// Finder decides: the queue reads and checks the contents itself.
  public var isNZB: Bool {
    pathExtension.lowercased() == "nzb"
  }
}
