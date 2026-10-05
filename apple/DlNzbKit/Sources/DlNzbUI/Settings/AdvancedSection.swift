import DlNzbKit
import SwiftUI

/// The Advanced pane and section: the pre-flight scan, recovery data, the
/// speed limit, certificate checking, retries, fsync, and the import, reset
/// and log buttons. The app supplies the buttons' platform parts (an open
/// panel for the import, Console or a log view for Show Logs).
public struct AdvancedSection: View {
  @Bindable var settings: SettingsStore
  let importCLIConfig: (() -> Void)?
  let showLogs: (() -> Void)?

  @State private var confirmingReset = false

  /// - Parameters:
  ///   - importCLIConfig: Import from dl-nzb CLI…; nil hides the button (iOS).
  ///   - showLogs: Show Logs; nil hides the button.
  public init(settings: SettingsStore, importCLIConfig: (() -> Void)? = nil, showLogs: (() -> Void)? = nil) {
    self.settings = settings
    self.importCLIConfig = importCLIConfig
    self.showLogs = showLogs
  }

  public var body: some View {
    Group {
      Section {
        Picker("Check availability before downloading", selection: $settings.preflight) {
          ForEach(Preflight.allCases, id: \.self) { mode in
            Text(mode.title).tag(mode)
          }
        }
        #if os(iOS)
          // The label fills a phone's row; a menu would drop its value onto a
          // second line. Settings' own idiom for a choice is a list one level down.
          .pickerStyle(.navigationLink)
        #endif
        Toggle("Download all recovery files up front", isOn: $settings.downloadAllRecoveryUpFront)
      } header: {
        Text("Downloading")
      } footer: {
        Text("Recovery files are otherwise fetched only when something is missing.")
      }

      Section {
        Toggle("Limit download speed", isOn: $settings.limitsSpeed)
        if settings.limitsSpeed {
          LabeledContent("Maximum speed") {
            HStack(spacing: 4) {
              TextField("Maximum speed", value: $settings.speedLimitMegabytesPerSecond, format: .number.precision(.fractionLength(0...1)))
                .labelsHidden()
                .multilineTextAlignment(.trailing)
                .frame(maxWidth: 80)
                #if os(iOS)
                  .keyboardType(.decimalPad)
                #endif
              Text(verbatim: "MB/s")
                .foregroundStyle(.secondary)
            }
          }
        }
      }

      Section {
        Toggle("Verify server certificate", isOn: $settings.verifyCertificate)
        Stepper(value: $settings.retryAttempts, in: ServerSettings.retryRange) {
          LabeledContent("Retry attempts") {
            Text(Format.count(settings.retryAttempts))
              .monospacedDigit()
          }
        }
        Toggle("Flush files to disk when finished", isOn: $settings.flushFilesWhenFinished)
      } header: {
        Text("Connection and Disk")
      }

      Section {
        if let importCLIConfig {
          Button("Import from dl-nzb CLI…", action: importCLIConfig)
        }
        if let showLogs {
          Button("Show Logs", action: showLogs)
        }
        Button("Reset All Settings", role: .destructive) { confirmingReset = true }
          .confirmationDialog("Reset All Settings?", isPresented: $confirmingReset, titleVisibility: .visible) {
            Button("Reset All Settings", role: .destructive) { settings.resetAll() }
            Button("Cancel", role: .cancel) {}
          } message: {
            Text("Your server password is removed from the keychain too. Your downloads stay where they are.")
          }
      }
    }
  }
}

#Preview("Advanced") {
  Form {
    AdvancedSection(settings: .preview(), importCLIConfig: {}, showLogs: {})
  }
  .formStyle(.grouped)
  .frame(width: 520, height: 560)
}
