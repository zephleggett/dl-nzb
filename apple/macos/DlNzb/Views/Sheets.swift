import AppKit
import DlNzbKit
import DlNzbUI
import SwiftUI

/// First launch without a server: the Server pane's fields, Test Connection,
/// and the CLI import, in a sheet over the window.
struct OnboardingSheet: View {
  @Environment(MacApp.self) private var app
  @Environment(SettingsStore.self) private var settings

  var body: some View {
    VStack(spacing: 0) {
      // No icon: the sheet hangs from dl-nzb's own window, and the room is
      // better spent keeping the whole form in view at the default size.
      VStack(spacing: 4) {
        Text("Connect to Your Usenet Server")
          .font(.title2.weight(.semibold))
        Text("dl-nzb downloads from the server your Usenet provider gave you.")
          .foregroundStyle(.secondary)
          .multilineTextAlignment(.center)
      }
      .padding(.top, 22)
      .padding(.horizontal, 24)

      Form {
        ServerSection(settings: settings, testConnection: app.model.testConnection)
      }
      .formStyle(.grouped)
      .scrollDisabled(true)

      HStack {
        if FileActions.cliConfigMayExist {
          Button("Import from dl-nzb CLI…") { app.importCLIConfig() }
        }
        Spacer()
        Button("Not Now") { app.isOnboarding = false }
          .keyboardShortcut(.cancelAction)
        Button("Continue") {
          settings.savePasswordNow()
          app.isOnboarding = false
        }
        .keyboardShortcut(.defaultAction)
        .disabled(!settings.hasServer)
      }
      .padding(.horizontal, 20)
      .padding(.bottom, 20)
    }
    .frame(width: 520)
    .fixedSize(horizontal: false, vertical: true)
  }
}

/// Enter Password… on a Password Required row. The download is done; the
/// password lets extraction finish.
struct PasswordSheet: View {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue
  let request: ItemRequest
  @State private var password = ""
  /// The lock grows with the text beside it.
  @ScaledMetric(relativeTo: .headline) private var lockSize: CGFloat = 28

  var body: some View {
    VStack(alignment: .leading, spacing: 16) {
      HStack(alignment: .top, spacing: 14) {
        Image(systemName: "lock.fill")
          .font(.system(size: lockSize))
          .foregroundStyle(.secondary)
          .frame(width: lockSize + 8)
          .accessibilityHidden(true)
        VStack(alignment: .leading, spacing: 4) {
          Text("Password Required")
            .font(.headline)
            .accessibilityAddTraits(.isHeader)
          // Breakable on screen, so a long name wraps between its parts;
          // VoiceOver hears it as words.
          Text("“\(ReleaseText.breakable(request.name))” is encrypted. dl-nzb extracts it once the password is right.")
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityLabel("“\(ReleaseText.spoken(request.name))” is encrypted. dl-nzb extracts it once the password is right.")
          if wasRejected {
            Text(StatusText.passwordRejected)
              .foregroundStyle(StatusTone.bad.emphasisStyle)
          }
        }
      }
      SecureField("Password", text: $password)
        .onSubmit(extract)
      HStack {
        Spacer()
        Button("Cancel", role: .cancel) { app.passwordRequest = nil }
          .keyboardShortcut(.cancelAction)
        Button("Extract", action: extract)
          .keyboardShortcut(.defaultAction)
          .disabled(password.isEmpty)
      }
    }
    .padding(20)
    .frame(width: 420)
  }

  private var wasRejected: Bool {
    request.ids.first.flatMap(queue.item)?.passwordRejected ?? false
  }

  private func extract() {
    guard !password.isEmpty else { return }
    app.providePassword(password, for: request)
  }
}
