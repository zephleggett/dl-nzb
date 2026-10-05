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
/// Shortcuts follow the apps people know: ⌘O opens; ⇧⌘R is Show in Finder
/// (Music); ⌘↓ and ⌘Y are Finder's Open and Quick Look; ⌘. stops, as in
/// Safari; ⌘R retries, Safari's reload; ⌫ and ⌘⌫ are Finder's; ⌘I shows the
/// inspector, Finder's Get Info. Pause All and Resume All take ⌥⌘P and ⌥⌘R,
/// leaving ⌘P (Print) alone.
struct AppCommands: Commands {
  let app: MacApp
  @FocusedValue(\.downloadsWindow) private var downloadsWindow

  /// The selection, when the main window is key and nothing modal is up.
  private var ids: Set<DownloadItem.ID> {
    downloadsWindow == true && !app.isPresentingModal ? app.selection : []
  }

  var body: some Commands {
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
      Button(app.inspectorToggleTitle) { app.showsInspector.toggle() }
        .keyboardShortcut("i")
        .disabled(app.queue.items.isEmpty)
      Divider()
    }

    CommandMenu("Downloads") {
      let queue = app.queue
      Button("Pause All") { queue.pauseAll() }
        .keyboardShortcut("p", modifiers: [.command, .option])
        .disabled(!queue.canPauseAll)
      Button("Resume All") { queue.resumeAll() }
        .keyboardShortcut("r", modifiers: [.command, .option])
        .disabled(!queue.canResumeAll)

      Divider()

      DownloadItemMenu(app: app, ids: ids, inMenuBar: true)
      Button("Remove Finished Downloads") { queue.removeAllFinished() }
        .disabled(!queue.items.contains(where: \.isFinished))
    }

    CommandGroup(replacing: .help) {
      Button("Acknowledgements") { app.openAcknowledgements() }
      if let url = URL(string: "https://github.com/zephleggett/dl-nzb") {
        Link("dl-nzb on GitHub", destination: url)
      }
    }
  }
}
