import DlNzbKit
import DlNzbUI
import SwiftUI

/// First launch without a server: the details once, with Test Connection.
/// Not Now leaves the sheet; downloads then wait until a server is added in
/// Settings, which the list says.
struct OnboardingSheet: View {
  @Environment(AppRuntime.self) private var runtime
  @Environment(\.dismiss) private var dismiss

  var body: some View {
    NavigationStack {
      Form {
        Section {
          VStack(alignment: .leading, spacing: 8) {
            Text("Connect to Your Usenet Server")
              .font(.title2.weight(.bold))
              .accessibilityAddTraits(.isHeader)
            Text("dl-nzb downloads from your own Usenet provider. Enter the details they gave you.")
              .foregroundStyle(.secondary)
          }
          .fixedSize(horizontal: false, vertical: true)
          .listRowBackground(Color.clear)
          .listRowInsets(EdgeInsets(top: 8, leading: 4, bottom: 8, trailing: 4))
        }
        ServerSection(settings: runtime.settings) {
          try await runtime.model.testConnection()
        }
      }
      .formStyle(.grouped)
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("Not Now") { dismiss() }
        }
        ToolbarItem(placement: .confirmationAction) {
          Button("Continue") {
            runtime.settings.savePasswordNow()
            dismiss()
          }
          .buttonStyle(.glassProminent)
          .disabled(!runtime.settings.hasServer)
        }
      }
    }
    .interactiveDismissDisabled(!runtime.settings.hasServer)
  }
}
