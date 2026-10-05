import DlNzbKit
import Foundation
import SwiftUI

/// One third-party component and its licence, for the Acknowledgements screen.
public struct Acknowledgement: Identifiable, Codable, Sendable, Equatable {
  public var name: String
  public var version: String?
  /// "MIT", "Apache-2.0", "MIT OR Apache-2.0".
  public var licence: String
  /// The licence's full text.
  public var text: String
  public var url: URL?

  public var id: String { name + (version ?? "") }

  public init(name: String, version: String? = nil, licence: String, text: String, url: URL? = nil) {
    self.name = name
    self.version = version
    self.licence = licence
    self.text = text
    self.url = url
  }

  /// Paragraph 2 of unRAR's licence, which permits its use in any software as
  /// long as this paragraph is reproduced in full, from "UnRAR source code".
  /// Verbatim from unrar_sys's vendor/unrar/license.txt.
  public static let unrarNotice =
    "UnRAR source code may be used in any software to handle RAR archives without limitations free of charge, but cannot be used to develop RAR (WinRAR) compatible archiver and to re-create RAR compression algorithm, which is proprietary. Distribution of modified UnRAR source code in separate form or as a part of other software is permitted, provided that full text of this paragraph, starting from \"UnRAR source code\" words, is included in license, or in documentation if license is not available, and in source code comments of resulting package."

  public static let par2rsURL = URL(string: "https://github.com/zephleggett/par2-rs")
  public static let dlNzbURL = URL(string: "https://github.com/zephleggett/dl-nzb")

  /// The Rust crates the engine links, with their licences. The FFI build
  /// regenerates DlNzbUI/Resources/rust-crates.json (an array of these, as
  /// JSON) from `cargo about` or similar; until then it is empty. Read once:
  /// SwiftUI makes the view, and so would read the file, far more often than
  /// it is shown.
  public static let bundledRustCrates: [Acknowledgement] = {
    guard let url = Bundle.module.url(forResource: "rust-crates", withExtension: "json"),
      let data = try? Data(contentsOf: url)
    else { return [] }
    do {
      return try JSONDecoder().decode([Acknowledgement].self, from: data)
    } catch {
      Log.app.error("rust-crates.json could not be read: \(error.localizedDescription, privacy: .public)")
      return []
    }
  }()
}

/// dl-nzb's own credits, the unRAR paragraph its licence requires, and every
/// open-source component with its licence. Both apps show it from Settings.
public struct AcknowledgementsView: View {
  let components: [Acknowledgement]

  /// - Parameter additional: components the app links itself (Sparkle on the
  ///   Mac's direct build), listed with the engine's Rust crates.
  public init(additional: [Acknowledgement] = []) {
    self.components = (Acknowledgement.bundledRustCrates + additional).sorted {
      $0.name.localizedStandardCompare($1.name) == .orderedAscending
    }
  }

  init(components: [Acknowledgement]) {
    self.components = components
  }

  public var body: some View {
    Form {
      Section {
        Text("dl-nzb and its PAR2 engine, par2-rs, are written by Zeph Leggett. par2-rs verifies and repairs downloads in pure Rust, with no other tools.")
          .fixedSize(horizontal: false, vertical: true)
        if let url = Acknowledgement.par2rsURL {
          Link("par2-rs on GitHub", destination: url)
        }
        if let url = Acknowledgement.dlNzbURL {
          Link("dl-nzb on GitHub", destination: url)
        }
      } header: {
        Text("dl-nzb")
      }

      Section {
        Text(Acknowledgement.unrarNotice)
          .font(.callout)
          .textSelection(.enabled)
          .fixedSize(horizontal: false, vertical: true)
      } header: {
        Text("UnRAR")
      } footer: {
        Text("RAR extraction uses the UnRAR source code by Alexander Roshal.")
      }

      Section("Open Source Components") {
        if components.isEmpty {
          Text("The list of components is added when the engine is built.")
            .foregroundStyle(.secondary)
        } else {
          ForEach(components) { component in
            DisclosureGroup {
              Text(component.text)
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            } label: {
              LabeledContent {
                Text(component.licence)
              } label: {
                Text(component.version.map { "\(component.name) \($0)" } ?? component.name)
              }
            }
          }
        }
      }
    }
    .formStyle(.grouped)
    .navigationTitle("Acknowledgements")
  }
}

#Preview("Acknowledgements") {
  AcknowledgementsView(components: [
    Acknowledgement(name: "tokio", version: "1.47.1", licence: "MIT", text: "Copyright (c) Tokio Contributors\n\nPermission is hereby granted…"),
    Acknowledgement(name: "Sparkle", version: "2.10.0", licence: "MIT", text: "Copyright (c) 2006-2013 Andy Matuschak…"),
  ])
  .frame(width: 560, height: 640)
}

#Preview("No component list yet") {
  AcknowledgementsView(components: [])
    .frame(width: 560, height: 520)
}
