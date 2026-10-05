import DlNzbKit
import DlNzbUI
import SwiftUI

/// Settings as a grouped form in a sheet: General (where files go, starting,
/// clearing, notifying, cellular), then the Server, Processing and Advanced
/// sections both apps share, and Acknowledgements. Changes apply as they are
/// made; Done only closes the sheet.
struct SettingsSheet: View {
  static let transitionID = "settings"

  @Environment(AppRuntime.self) private var runtime
  @Environment(\.dismiss) private var dismiss

  var body: some View {
    NavigationStack {
      Form {
        GeneralSection()
        ServerSection(settings: runtime.settings) {
          try await runtime.model.testConnection()
        }
        ProcessingSection(settings: runtime.settings)
        AdvancedSection(settings: runtime.settings)
        Section {
          NavigationLink("Acknowledgements") {
            AcknowledgementsView()
          }
        } footer: {
          Text(versionLine)
        }
      }
      .formStyle(.grouped)
      .navigationTitle("Settings")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) {
          Button(role: .confirm) { dismiss() }
        }
      }
    }
  }

  /// "dl-nzb 0.7.0 (12)".
  private var versionLine: String { "dl-nzb \(AppVersion.display)" }
}

/// Where files go and how the queue behaves.
private struct GeneralSection: View {
  @Environment(AppRuntime.self) private var runtime
  @Environment(\.openURL) private var openURL

  var body: some View {
    @Bindable var settings = runtime.settings
    @Bindable var phone = runtime.phoneSettings
    Section {
      LabeledContent("Downloads") {
        Text(FilesLocation.displayPath(of: settings.downloadFolder))
          .multilineTextAlignment(.trailing)
      }
      Button("Show in Files") {
        FilesLocation.ensureDownloadFolder(settings.downloadFolder)
        if let url = FilesLocation.filesAppURL(for: settings.downloadFolder) { openURL(url) }
      }
    } header: {
      Text("General")
    } footer: {
      Text("Each download gets its own folder.")
    }

    Section {
      Toggle("Start downloads automatically", isOn: $settings.startAutomatically)
      Picker("Remove finished downloads", selection: $settings.retention) {
        ForEach(RetentionPolicy.allCases, id: \.self) { policy in
          Text(policy.title).tag(policy)
        }
      }
      Toggle("Notify when downloads finish", isOn: $settings.notifyWhenFinished)
        .onChange(of: settings.notifyWhenFinished) { _, isOn in
          if isOn { Task { await runtime.notifier.requestAuthorizationIfNeeded() } }
        }
      Toggle("Allow downloads on cellular", isOn: $phone.allowsCellular)
        .onChange(of: phone.allowsCellular) { runtime.gate.reevaluate() }
    } footer: {
      Text("When off, dl-nzb asks before using cellular data.")
    }
  }
}
