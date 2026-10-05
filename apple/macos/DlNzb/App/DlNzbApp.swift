import AppKit
import DlNzbKit
import DlNzbUI
import SwiftUI

/// dl-nzb for Mac: one main window, Settings and an Acknowledgements window.
/// File opens and quitting go through `AppDelegate`; the menu bar item is
/// AppKit's (`StatusMenu`); everything else goes through `MacApp.shared`.
@main
struct DlNzbApp: App {
  @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
  @State private var app = MacApp.shared

  var body: some Scene {
    Window("dl-nzb", id: "main") {
      MainWindow()
        .environment(of: app)
        .onAppear { delegate.target = app }
    }
    // Tall enough at first launch for the onboarding sheet to hang inside it.
    .defaultSize(width: 780, height: 620)
    .windowResizability(.contentMinSize)
    .defaultLaunchBehavior(.presented)
    .commands { AppCommands(app: app) }

    Window("Acknowledgements", id: "acknowledgements") {
      AcknowledgementsView(additional: Self.linkedComponents)
        .frame(minWidth: 480, idealWidth: 560, minHeight: 420, idealHeight: 640)
    }
    .defaultSize(width: 560, height: 640)
    .windowResizability(.contentMinSize)
    .defaultLaunchBehavior(.suppressed)
    .restorationBehavior(.disabled)
    .commandsRemoved()

    Settings {
      SettingsView()
        .environment(of: app)
    }
  }

  /// Components the app links beyond the engine's Rust crates.
  private static var linkedComponents: [Acknowledgement] {
    #if DIRECT
      [UpdateController.acknowledgement]
    #else
      []
    #endif
  }
}

extension View {
  /// The app, its settings and its queue, which every scene's views read.
  func environment(of app: MacApp) -> some View {
    environment(app).environment(app.settings).environment(app.queue)
  }
}
