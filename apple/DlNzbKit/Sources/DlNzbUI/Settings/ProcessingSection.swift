import DlNzbKit
import SwiftUI

/// What happens once the files arrive: the Processing pane and section.
public struct ProcessingSection: View {
  @Bindable var settings: SettingsStore

  public init(settings: SettingsStore) {
    self.settings = settings
  }

  public var body: some View {
    Section {
      Toggle("Repair with PAR2", isOn: $settings.repairWithPar2)
      Toggle("Extract archives", isOn: $settings.extractArchives)
      Toggle("Delete archives after extracting", isOn: $settings.deleteArchivesAfterExtracting)
        .disabled(!settings.extractArchives)
      Toggle("Delete PAR2 files after repairing", isOn: $settings.deletePar2AfterRepairing)
        .disabled(!settings.repairWithPar2)
      Toggle("Rename obfuscated files", isOn: $settings.renameObfuscatedFiles)
    } header: {
      Text("After Downloading")
    } footer: {
      Text("Changes apply to downloads that start afterwards.")
    }
  }
}

#Preview("Processing") {
  Form {
    ProcessingSection(settings: .preview())
  }
  .formStyle(.grouped)
  .frame(width: 520, height: 320)
}
