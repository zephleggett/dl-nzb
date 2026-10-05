import AppKit
import DlNzbKit
import DlNzbUI
import SwiftUI

/// The Settings window: General, Server, Processing and Advanced as toolbar
/// panes, each a grouped form a fixed width wide. The Server, Processing and
/// Advanced sections are the ones the iPhone app and onboarding use too.
struct SettingsView: View {
  @Environment(MacApp.self) private var app

  var body: some View {
    @Bindable var app = app
    TabView(selection: $app.settingsPane) {
      Tab("General", systemImage: "gearshape", value: SettingsPane.general) {
        GeneralPane()
      }
      Tab("Server", systemImage: "server.rack", value: SettingsPane.server) {
        ServerPane()
      }
      Tab("Processing", systemImage: "archivebox", value: SettingsPane.processing) {
        ProcessingPane()
      }
      Tab("Advanced", systemImage: "gearshape.2", value: SettingsPane.advanced) {
        AdvancedPane()
      }
    }
    .frame(width: SettingsLayout.width)
  }
}

enum SettingsLayout {
  static let width: CGFloat = 520
}

/// A pane's grouped form, sized to its content so the window fits each pane.
struct SettingsPaneForm<Content: View>: View {
  @ViewBuilder let content: Content

  var body: some View {
    Form {
      content
    }
    .formStyle(.grouped)
    .scrollDisabled(true)
    .fixedSize(horizontal: false, vertical: true)
    // The grouped form leaves less below its last section than above its first.
    .padding(.bottom, 10)
  }
}

// MARK: General

struct GeneralPane: View {
  @Environment(MacApp.self) private var app
  @Environment(SettingsStore.self) private var settings

  var body: some View {
    @Bindable var settings = settings
    SettingsPaneForm {
      Section {
        LabeledContent("Download folder") {
          FolderLabel(url: settings.downloadFolder)
            .labelStyle(.titleAndIcon)
        }
        HStack {
          Spacer()
          if !isDefaultFolder {
            Button("Use Downloads Folder") { settings.useDefaultDownloadFolder() }
          }
          Button("Choose…") { app.chooseDownloadFolder() }
        }
      } footer: {
        Text("Each download gets a folder of its own in here.")
      }

      Section {
        Toggle("Start downloads automatically", isOn: $settings.startAutomatically)
        Picker("Remove finished downloads", selection: $settings.retention) {
          ForEach(RetentionPolicy.allCases, id: \.self) { policy in
            Text(policy.title).tag(policy)
          }
        }
      } footer: {
        Text(settings.startAutomatically ? "New downloads start when their turn comes." : "New downloads wait until you start them.")
      }

      Section {
        Toggle("Notify when downloads finish", isOn: $settings.notifyWhenFinished)
        Toggle("Prevent sleep while downloading", isOn: $settings.preventSleep)
        Toggle("Show in menu bar", isOn: $settings.showInMenuBar)
      }

      #if DIRECT
        UpdatesSection(updates: app.updates)
      #endif
    }
  }

  private var isDefaultFolder: Bool {
    settings.downloadFolder.standardizedFileURL == AppPaths.defaultDownloadFolder.standardizedFileURL
  }
}

#if DIRECT
  /// Sparkle's settings, in the Direct flavour only.
  struct UpdatesSection: View {
    @Bindable var updates: UpdateController

    var body: some View {
      Section("Updates") {
        if updates.isAvailable {
          Toggle("Check for updates automatically", isOn: $updates.automaticallyChecks)
          LabeledContent("dl-nzb \(AppVersion.display)") {
            Button("Check Now") { updates.checkForUpdates() }
              .disabled(!updates.canCheckForUpdates)
          }
        } else {
          LabeledContent("dl-nzb \(AppVersion.display)") {
            Text("This build does not update itself.")
              .foregroundStyle(.secondary)
          }
        }
      }
    }
  }
#endif

// MARK: Server

struct ServerPane: View {
  @Environment(MacApp.self) private var app
  @Environment(SettingsStore.self) private var settings

  var body: some View {
    SettingsPaneForm {
      ServerSection(settings: settings, testConnection: app.model.testConnection, committed: app.model.serverSettingsCommitted)
      if let problem = app.model.settingsProblem {
        Section {
          Label {
            Text(problem)
              .foregroundStyle(StatusTone.warning.emphasisStyle)
          } icon: {
            Image(systemName: "exclamationmark.triangle.fill")
              .foregroundStyle(StatusTone.warning.glyphStyle)
          }
        }
      }
    }
    // Leaving the pane, or closing Settings, is when the user has finished
    // typing: a server problem is tried again with what they entered.
    .onDisappear { app.model.serverSettingsCommitted() }
  }
}

// MARK: Processing

struct ProcessingPane: View {
  @Environment(SettingsStore.self) private var settings

  var body: some View {
    SettingsPaneForm {
      ProcessingSection(settings: settings)
    }
  }
}

// MARK: Advanced

struct AdvancedPane: View {
  @Environment(MacApp.self) private var app
  @Environment(SettingsStore.self) private var settings

  var body: some View {
    SettingsPaneForm {
      AdvancedSection(settings: settings, importCLIConfig: { app.importCLIConfig() }, showLogs: { LogExport.show() })
      Section {
        Button("Acknowledgements") { app.openAcknowledgements() }
      } footer: {
        Text("dl-nzb, par2-rs, UnRAR and the open-source components inside.")
      }
    }
  }
}
