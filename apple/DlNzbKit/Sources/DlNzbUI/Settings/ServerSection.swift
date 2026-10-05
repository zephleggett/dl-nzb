import DlNzbKit
import SwiftUI

/// What Test Connection last found, shown inline under the button.
public enum ConnectionTestState: Equatable, Sendable {
  case idle
  case testing
  case success(latencyMilliseconds: Int, tls: Bool)
  /// The engine's own sentence: wrong password, unknown host, TLS failed.
  case failure(String)

  /// Runs a test and turns its outcome into a state.
  public static func run(_ test: @MainActor () async throws -> ServerCheck) async -> ConnectionTestState {
    do {
      let check = try await test()
      return .success(latencyMilliseconds: check.latencyMilliseconds, tls: check.tls)
    } catch let error as EngineError {
      return .failure(error.message)
    } catch is CancellationError {
      return .idle
    } catch {
      return .failure(error.localizedDescription)
    }
  }

  /// "Connected · 38 ms", "Connected without encryption · 38 ms".
  public var text: String? {
    switch self {
    case .idle: nil
    case .testing: "Connecting…"
    case .success(let latency, let tls):
      "\(tls ? "Connected" : "Connected without encryption") · \(Format.count(latency)) ms"
    case .failure(let message): message
    }
  }
}

/// Host, port, SSL, account, connections and Test Connection: the Server pane
/// on the Mac, the Server section on iPhone and iPad, and the first-launch
/// sheet. Edits go straight to the settings store.
public struct ServerSection: View {
  @Bindable var settings: SettingsStore
  let testConnection: @MainActor () async throws -> ServerCheck
  let committed: (@MainActor () -> Void)?

  @State private var testState: ConnectionTestState = .idle
  @State private var testTask: Task<Void, Never>?

  /// - Parameters:
  ///   - testConnection: usually `AppModel.testConnection`.
  ///   - committed: Return in the host, username or password field: the
  ///     user has finished typing. Usually `AppModel.serverSettingsCommitted`.
  public init(
    settings: SettingsStore, testConnection: @escaping @MainActor () async throws -> ServerCheck, committed: (@MainActor () -> Void)? = nil
  ) {
    self.settings = settings
    self.testConnection = testConnection
    self.committed = committed
  }

  public var body: some View {
    Group {
      Section("Server") {
        LabeledField("Host") {
          TextField("Host", text: $settings.host, prompt: Text(verbatim: "news.example.com"))
            #if os(iOS)
              .textContentType(.URL)
              .keyboardType(.URL)
              .textInputAutocapitalization(.never)
            #endif
            .autocorrectionDisabled()
            .onSubmit { committed?() }
        }
        LabeledField("Port") {
          TextField("Port", value: $settings.port, format: .number.grouping(.never), prompt: Text(verbatim: "563"))
            #if os(iOS)
              .keyboardType(.numberPad)
            #endif
        }
        Toggle("Use SSL/TLS", isOn: $settings.useSSL)
      }

      Section("Account") {
        LabeledField("Username") {
          TextField("Username", text: $settings.username)
            #if os(iOS)
              .textContentType(.username)
              .textInputAutocapitalization(.never)
            #endif
            .autocorrectionDisabled()
            .onSubmit { committed?() }
        }
        LabeledField("Password") {
          SecureField("Password", text: $settings.password)
            #if os(iOS)
              .textContentType(.password)
            #endif
            .onSubmit {
              settings.savePasswordNow()
              committed?()
            }
        }
      }

      Section {
        Stepper(value: $settings.connections, in: ServerSettings.connectionRange) {
          LabeledContent("Connections") {
            Text(Format.count(settings.connections))
              .monospacedDigit()
          }
        }
      } footer: {
        Text("Your provider sets the maximum.")
      }

      Section {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
          Button("Test Connection", action: runTest)
            .disabled(!settings.hasServer || testState == .testing)
          ConnectionTestResult(state: testState)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
      }
    }
    .onChange(of: settings.serverSettings) { resetTest() }
    .onChange(of: settings.password) { resetTest() }
    .onDisappear { testTask?.cancel() }
  }

  private func runTest() {
    settings.savePasswordNow()
    testTask?.cancel()
    testState = .testing
    let test = testConnection
    testTask = Task {
      let result = await ConnectionTestState.run(test)
      guard !Task.isCancelled else { return }
      testState = result
    }
  }

  private func resetTest() {
    testTask?.cancel()
    testTask = nil
    testState = .idle
  }
}

/// The inline result beside Test Connection.
public struct ConnectionTestResult: View {
  let state: ConnectionTestState

  public init(state: ConnectionTestState) {
    self.state = state
  }

  public var body: some View {
    switch state {
    case .idle:
      EmptyView()
    case .testing:
      HStack(spacing: 6) {
        ProgressView()
          .controlSize(.small)
        Text(state.text ?? "")
          .foregroundStyle(.secondary)
      }
    case .success:
      result(state.text ?? "", systemImage: "checkmark.circle.fill", tone: .good)
        .monospacedDigit()
    case .failure(let message):
      result(message, systemImage: "xmark.circle.fill", tone: .bad)
        .fixedSize(horizontal: false, vertical: true)
    }
  }

  /// The glyph in the tone's colour, the words in its readable shade.
  private func result(_ text: String, systemImage: String, tone: StatusTone) -> some View {
    Label {
      Text(text)
        .foregroundStyle(tone.emphasisStyle)
    } icon: {
      Image(systemName: systemImage)
        .foregroundStyle(tone.glyphStyle)
    }
  }
}

/// A text field with its label beside it on both platforms: the Mac's grouped
/// form does this by itself, an iPhone form needs LabeledContent.
struct LabeledField<Field: View>: View {
  let title: LocalizedStringKey
  @ViewBuilder let field: Field

  init(_ title: LocalizedStringKey, @ViewBuilder field: () -> Field) {
    self.title = title
    self.field = field()
  }

  var body: some View {
    #if os(iOS)
      LabeledContent(title) {
        field
          .multilineTextAlignment(.trailing)
          .labelsHidden()
      }
    #else
      field
    #endif
  }
}

#Preview("Server") {
  Form {
    ServerSection(settings: .preview()) {
      try await Task.sleep(for: .milliseconds(600))
      return ServerCheck(greeting: "200 news.example.com", tls: true, latencyMilliseconds: 38)
    }
  }
  .formStyle(.grouped)
  .frame(width: 520, height: 520)
}

#Preview("Test results") {
  Form {
    ConnectionTestResult(state: .testing)
    ConnectionTestResult(state: .success(latencyMilliseconds: 38, tls: true))
    ConnectionTestResult(state: .failure(EngineError(.auth).message))
    ConnectionTestResult(state: .failure("No server called news.example.invalid could be found. Check the host name."))
  }
  .formStyle(.grouped)
  .frame(width: 520, height: 300)
}
