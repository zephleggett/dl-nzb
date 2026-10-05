import DlNzbKit
import SwiftUI

@main
struct DlNzbApp: App {
  @State private var runtime = AppRuntime()
  @Environment(\.scenePhase) private var scenePhase

  var body: some Scene {
    WindowGroup {
      RootView()
        .environment(runtime)
        // Files, Safari's downloads, the share sheet, AirDrop and Mail all
        // arrive here; the app declares the NZB document type and opens in place.
        .onOpenURL { url in
          Task { await runtime.open([url]) }
        }
        // `initial`: also as the window first appears, which launches the runtime.
        .onChange(of: scenePhase, initial: true) { _, phase in
          runtime.scenePhaseChanged(to: phase)
        }
    }
  }
}
