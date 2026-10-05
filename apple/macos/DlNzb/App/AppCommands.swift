import AppKit
import DlNzbKit
import SwiftUI

extension FocusedValues {
  /// Set while the main window is key, so the Downloads menu's item commands
  /// (and their plain-key shortcuts) never reach into Settings' text fields.
  @Entry var downloadsWindow: Bool?
}

/// The menu bar. Every toolbar command is here too.
///
/// It reads only `MacApp.menuState`, which changes when an item here would
/// change: SwiftUI rebuilds the whole main menu whenever what the commands
/// read changes, and a rebuild redraws an open menu.
///
/// Shortcuts follow the apps people know: ⌘O opens; ⇧⌘R is Show in Finder
/// (Music); ⌘↓ and ⌘Y are Finder's Open and Quick Look; ⌘. stops, as in
/// Safari; ⌘R retries, Safari's reload; ⌫ and ⌘⌫ are Finder's; ⌘I shows the
/// inspector, Finder's Get Info. Pause All and Resume All take ⌥⌘P and ⌥⌘R,
/// leaving ⌘P (Print) alone.
struct AppCommands: Commands {
  let app: MacApp
  @FocusedValue(\.downloadsWindow) private var downloadsWindow

  var body: some Commands {
    let state = app.menuState
    #if DIRECT
      CommandGroup(after: .appInfo) {
        Button("Check for Updates…") { app.updates.checkForUpdates() }
          .disabled(!app.updates.canCheckForUpdates)
      }
    #endif

    CommandGroup(replacing: .newItem) {
      Button("Open…") { app.presentOpenPanel() }
        .keyboardShortcut("o")
    }

    CommandGroup(after: .sidebar) {
      Button(state.inspectorToggleTitle) { app.showsInspector.toggle() }
        .keyboardShortcut("i")
        .disabled(!state.hasDownloads)
      Divider()
    }

    CommandMenu("Downloads") {
      let queue = app.queue
      Button("Pause All") { queue.pauseAll() }
        .keyboardShortcut("p", modifiers: [.command, .option])
        .disabled(!state.canPauseAll)
      Button("Resume All") { queue.resumeAll() }
        .keyboardShortcut("r", modifiers: [.command, .option])
        .disabled(!state.canResumeAll)

      Divider()

      // The selection's, while the main window is key.
      DownloadItemMenu(app: app, commands: downloadsWindow == true ? state.selection : .none, inMenuBar: true)
      Button("Remove Finished Downloads") { queue.removeAllFinished() }
        .disabled(!state.hasFinished)
    }

    CommandGroup(replacing: .help) {
      Button("Acknowledgements") { app.openAcknowledgements() }
      if let url = URL(string: "https://github.com/zephleggett/dl-nzb") {
        Link("dl-nzb on GitHub", destination: url)
      }
    }
  }
}
