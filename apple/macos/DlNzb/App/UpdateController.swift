#if DIRECT
  import AppKit
  import DlNzbKit
  import DlNzbUI
  import Observation
  @preconcurrency import Sparkle

  /// Updates for the Direct flavour, through Sparkle: a check a day, Check for
  /// Updates… in the app menu, and the setting in General. Only a build with a
  /// feed and a public key has them, so a local build never offers to replace
  /// itself. The app is sandboxed, so Sparkle installs through its Installer
  /// Launcher service (Info.plist and dl-nzb-Direct.entitlements).
  @MainActor
  @Observable
  final class UpdateController {
    var isAvailable: Bool { controller != nil }
    /// False while a check is running.
    private(set) var canCheckForUpdates = false

    /// Sparkle's own setting, read live. Written only from Settings, as
    /// Sparkle asks; writing it at launch would pin Info.plist's default.
    var automaticallyChecks: Bool {
      get {
        access(keyPath: \.automaticallyChecks)
        return controller?.updater.automaticallyChecksForUpdates ?? false
      }
      set {
        withMutation(keyPath: \.automaticallyChecks) { controller?.updater.automaticallyChecksForUpdates = newValue }
      }
    }

    @ObservationIgnored private var controller: SPUStandardUpdaterController?
    @ObservationIgnored private var canCheckObservation: NSKeyValueObservation?

    init(enabled: Bool = true) {
      let feed = Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") as? String ?? ""
      let key = Bundle.main.object(forInfoDictionaryKey: "SUPublicEDKey") as? String ?? ""
      guard enabled, !feed.isEmpty, !key.isEmpty else {
        Log.mac.info("updates are off: this build has no update feed")
        return
      }
      let controller = SPUStandardUpdaterController(startingUpdater: false, updaterDelegate: nil, userDriverDelegate: nil)
      do {
        try controller.updater.start()
      } catch {
        Log.mac.error("the updater did not start: \(error.localizedDescription, privacy: .public)")
        return
      }
      self.controller = controller
      canCheckObservation = controller.updater.observe(\.canCheckForUpdates, options: [.initial, .new]) { [weak self] _, change in
        let value = change.newValue ?? false
        MainActor.assumeIsolated { self?.canCheckForUpdates = value }
      }
    }

    func checkForUpdates() {
      controller?.checkForUpdates(nil)
    }

    /// Sparkle's licence for Acknowledgements, from the bundle, and the
    /// version of the framework the app carries.
    static var acknowledgement: Acknowledgement {
      let text =
        Bundle.main.url(forResource: "Sparkle-LICENSE", withExtension: "txt")
        .flatMap { try? String(contentsOf: $0, encoding: .utf8) } ?? "MIT License. Copyright (c) 2006-2013 Andy Matuschak and the Sparkle contributors."
      let version = Bundle(for: SPUUpdater.self).object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
      return Acknowledgement(name: "Sparkle", version: version, licence: "MIT", text: text, url: URL(string: "https://sparkle-project.org"))
    }
  }
#endif
