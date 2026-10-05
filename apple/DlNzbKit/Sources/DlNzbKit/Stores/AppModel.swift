import Foundation
import Observation

/// The composition root. One of these exists for the life of each app: it
/// owns the settings, the queue and the engine, keeps the engine's settings in
/// step with the user's, and brings everything up and down.
///
/// The engine comes from a factory the app passes in, because only the app
/// links the Rust adapter:
///
/// ```swift
/// let model = AppModel { kind in
///   switch kind {
///   case .rust: RustEngine()
///   case .simulated: SimulatedEngine()
///   }
/// }
/// ```
///
/// Launching with `-simulate YES` asks for `.simulated`.
@MainActor
@Observable
public final class AppModel {
  public enum EngineKind: String, Sendable {
    case rust
    case simulated
  }

  public let settings: SettingsStore
  public let queue: DownloadQueue
  public let engineKind: EngineKind
  public private(set) var isLaunched = false
  /// The engine turned the settings down; one sentence for the Settings window.
  public private(set) var settingsProblem: String?

  @ObservationIgnored public let engine: any DownloadEngine
  @ObservationIgnored private let isLive: Bool
  @ObservationIgnored private var applyTask: Task<Void, Never>?
  /// The latest settings apply, so the next waits for it and the engine
  /// hears about changes in the order they were made.
  @ObservationIgnored private var applying: Task<Void, Never>?
  @ObservationIgnored private var appliedSettings: EngineSettings?

  /// - Parameters:
  ///   - simulate: Use the simulated engine; defaults to the `-simulate` launch argument.
  ///   - makeEngine: Builds the engine of the kind asked for.
  public init(
    settings: SettingsStore? = nil,
    storage: QueueStorage = .standard,
    simulate: Bool = AppModel.simulateRequested(),
    makeEngine: (EngineKind) -> any DownloadEngine
  ) {
    let settings = settings ?? SettingsStore()
    let kind: EngineKind = simulate ? .simulated : .rust
    let engine = makeEngine(kind)
    self.settings = settings
    self.engineKind = kind
    self.engine = engine
    self.queue = DownloadQueue(engine: engine, settings: settings, storage: storage)
    self.isLive = true
  }

  private init(preview settings: SettingsStore, queue: DownloadQueue) {
    self.settings = settings
    self.queue = queue
    self.engine = queue.engine
    self.engineKind = .simulated
    self.isLive = false
  }

  /// `-simulate YES` on the command line, or in the scheme's arguments.
  public nonisolated static func simulateRequested(defaults: UserDefaults = .standard) -> Bool {
    defaults.bool(forKey: "simulate")
  }

  /// Called once the app has finished launching; again is harmless. Restores
  /// the queue at once (so the list shows), gives the engine its settings
  /// (once the password is read from the Keychain), then lets downloads start.
  public func launch() {
    guard isLive, !isLaunched else { return }
    isLaunched = true
    Log.app.info("launching with the \(self.engineKind.rawValue, privacy: .public) engine")
    queue.restore()
    observeSettings()
    Task {
      await settings.passwordLoaded()
      await applySettings()
      queue.activate()
    }
  }

  /// Opens NZBs from anywhere: launches first if an open event beat the UI.
  public func open(_ urls: [URL]) async -> [AddResult] {
    launch()
    return await queue.add(urls: urls)
  }

  /// Test Connection with what the form holds now, saved or not. When it
  /// works and a server problem has paused the queue, the engine takes these
  /// settings at once and the queue tries again.
  public func testConnection() async throws -> ServerCheck {
    await settings.passwordLoaded()
    let check = try await engine.testConnection(settings.serverSettings, password: settings.password)
    if queue.unresolvedServerProblem != nil {
      await applySettings()
      queue.retryServer()
    }
    return check
  }

  /// The user has finished with the server settings: the Settings window
  /// closed, or they left its Server pane or sheet. The engine takes them now
  /// rather than after the typing pause, and if a server problem paused the
  /// queue and the settings changed since, the queue tries again. Typing
  /// alone never retries, so a half-typed password is not sent.
  public func serverSettingsCommitted() {
    guard isLive else { return }
    applyTask?.cancel()
    applyTask = nil
    Task {
      await applySettings()
      queue.serverSettingsCommitted()
    }
  }

  /// Reads a CLI config file the user picked (the Mac's open panel starts at
  /// `CLIConfig.defaultURL`), with the engine's reader (the CLI's own parser
  /// under `RustEngine`), and takes on its settings.
  @discardableResult
  public func importCLIConfig(from url: URL) async throws -> ImportedSettings {
    let imported = try await engine.importCLIConfig(from: url)
    settings.apply(imported)
    return imported
  }

  /// Everything the app does before it quits: jobs stop where they can
  /// continue, the list is saved, the password is written, the engine closes
  /// its connections. The app delegate awaits this before replying to
  /// `applicationShouldTerminate`.
  public func prepareForQuit() async {
    guard isLive else { return }
    Log.app.info("preparing to quit with \(self.queue.unfinishedCount) unfinished downloads")
    applyTask?.cancel()
    settings.savePasswordNow()
    await settings.keychainSettled()
    await queue.prepareForQuit()
    await engine.shutdown()
  }

  // MARK: Settings to the engine

  private func observeSettings() {
    withObservationTracking {
      _ = settings.engineSettings
    } onChange: { [weak self] in
      Task { @MainActor in
        self?.settingsChanged()
        self?.observeSettings()
      }
    }
  }

  /// Typing in a field changes a setting a character at a time; the engine
  /// hears about it once the typing stops.
  private func settingsChanged() {
    applyTask?.cancel()
    applyTask = Task { [weak self] in
      try? await Task.sleep(for: .milliseconds(400))
      guard !Task.isCancelled else { return }
      await self?.applySettings()
    }
  }

  /// Gives the engine the current settings, after any apply still under way.
  private func applySettings() async {
    let previous = applying
    let task = Task { [weak self] in
      await previous?.value
      await self?.applySettingsNow()
    }
    applying = task
    await task.value
  }

  private func applySettingsNow() async {
    let next = settings.engineSettings
    let previous = appliedSettings
    guard next != previous else { return }
    appliedSettings = next
    do {
      try await engine.apply(next)
      settingsProblem = nil
    } catch {
      settingsProblem = (error as? EngineError)?.message ?? error.localizedDescription
      Log.app.error("the engine turned the settings down: \(error.localizedDescription, privacy: .public)")
    }
    if next.speedLimitBytesPerSecond != previous?.speedLimitBytesPerSecond {
      await engine.setSpeedLimit(bytesPerSecond: next.speedLimitBytesPerSecond)
    }
    if let previous, previous.server != next.server || previous.password != next.password {
      queue.serverSettingsChanged()
    }
  }

  // MARK: Previews

  /// An app with fixed contents and nothing behind it, for previews.
  public static func preview(items: [DownloadItem] = PreviewData.items, settings: SettingsStore? = nil) -> AppModel {
    let settings = settings ?? .preview()
    return AppModel(preview: settings, queue: .preview(items: items, settings: settings))
  }
}
