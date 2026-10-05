import AppKit
import DlNzbKit
import DlNzbUI
import SwiftUI

/// dl-nzb for Mac: one main window, Settings, an Acknowledgements window and
/// an optional menu bar item. File opens and quitting go through
/// `AppDelegate`; everything else through `MacApp.shared`.
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

    MenuBarExtra(isInserted: showsInMenuBar) {
      MenuBarContent()
        .environment(of: app)
    } label: {
      MenuBarLabel(app: app)
        .onAppear { delegate.target = app }
    }
    .menuBarExtraStyle(.menu)
  }

  private var showsInMenuBar: Binding<Bool> {
    Binding(get: { app.settings.showInMenuBar }, set: { app.settings.showInMenuBar = $0 })
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

/// The menu bar glyph: the segmented arrow, filled while downloading. Both are
/// template images, so the menu bar tints them.
///
/// It reads the queue in its own body: read in the scene's, every progress
/// event would rebuild every scene.
struct MenuBarLabel: View {
  let app: MacApp
  @Environment(\.openWindow) private var openWindow

  var body: some View {
    let isActive = app.queue.activeCount > 0
    Image(isActive ? "MenuBarIconActive" : "MenuBarIcon")
      .accessibilityLabel(isActive ? "dl-nzb, downloading" : "dl-nzb")
      // The label exists from launch, so a file opened from Finder can bring
      // the window back even if it was never shown in this run.
      .onAppear { if app.openWindowAction == nil { app.openWindowAction = openWindow } }
  }
}
