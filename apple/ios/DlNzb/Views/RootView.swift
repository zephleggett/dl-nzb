import DlNzbKit
import DlNzbUI
import SwiftUI
import UniformTypeIdentifiers

/// The list and the selected download. At regular width (iPad, or a wide
/// window) they sit side by side; at compact width (iPhone, Slide Over, a
/// narrow Stage Manager window) the split view collapses into one navigation
/// stack, "Downloads", and a download's detail is pushed onto it. Size
/// classes decide, never the device.
///
/// The app-wide questions live here: the server problem, files that could not
/// be opened, duplicates, the cellular prompt and failed file operations.
struct RootView: View {
  @Environment(AppRuntime.self) private var runtime
  @Environment(\.horizontalSizeClass) private var sizeClass
  @Namespace private var settingsTransition

  var body: some View {
    @Bindable var runtime = runtime
    NavigationSplitView {
      DownloadsScreen(selection: $runtime.selection, settingsTransition: settingsTransition, isSplit: sizeClass == .regular)
        .navigationSplitViewColumnWidth(min: 340, ideal: 420, max: 560)
    } detail: {
      DetailColumn()
    }
    .navigationSplitViewStyle(.balanced)
    // Closing either sheet is when the server details count as entered: a
    // queue a server problem paused tries again with them then.
    .sheet(isPresented: $runtime.isShowingSettings, onDismiss: runtime.serverSettingsCommitted) {
      SettingsSheet()
        .navigationTransition(.zoom(sourceID: SettingsSheet.transitionID, in: settingsTransition))
    }
    .sheet(isPresented: $runtime.isShowingOnboarding, onDismiss: runtime.serverSettingsCommitted) {
      OnboardingSheet()
    }
    .fileImporter(isPresented: $runtime.isShowingImporter, allowedContentTypes: Self.importableTypes, allowsMultipleSelection: true) { result in
      switch result {
      case .success(let urls):
        Task { await runtime.open(urls) }
      case .failure(let error):
        AppLog.open.error("the file importer failed: \(error.localizedDescription, privacy: .public)")
      }
    }
    .onChange(of: runtime.router.lastAdded) { _, id in
      // Side by side, the new download shows at once; on an iPhone the list
      // stays where it is.
      if sizeClass == .regular, let id { runtime.selection = id }
    }
    .modifier(AppAlerts())
    .modifier(OutcomeFeedback())
  }

  /// NZBs, and XML in case another app claims the .nzb extension with a type
  /// of its own (the queue turns away anything that is not an NZB).
  static let importableTypes: [UTType] = [UTType(importedAs: "com.zephleggett.dl-nzb.nzb", conformingTo: .xml), .xml]
}

// The two views below read the list, which changes with every progress
// update; on their own, they keep those updates from redrawing the root.

/// The selected download, or what to do without one.
private struct DetailColumn: View {
  @Environment(AppRuntime.self) private var runtime

  var body: some View {
    if let id = runtime.selection, let item = runtime.queue.item(id) {
      LiveItem(item, in: runtime.queue) { DownloadDetailView(item: $0) }
    } else {
      ContentUnavailableView("Select a Download", systemImage: "arrow.down.circle", description: Text("Its progress, files and details appear here."))
    }
  }
}

/// Success when a download finishes; a warning when one fails or needs the user.
private struct OutcomeFeedback: ViewModifier {
  @Environment(AppRuntime.self) private var runtime

  func body(content: Content) -> some View {
    let items = runtime.queue.items
    content
      .sensoryFeedback(trigger: items.count(where: \.isFinished)) { old, new in new > old ? .success : nil }
      .sensoryFeedback(trigger: items.count { if case .failed = $0.state { true } else { $0.needsAttention } }) { old, new in
        new > old ? .warning : nil
      }
  }
}

/// Alerts that are not about one row.
private struct AppAlerts: ViewModifier {
  @Environment(AppRuntime.self) private var runtime
  @Environment(\.openURL) private var openURL

  func body(content: Content) -> some View {
    @Bindable var gate = runtime.gate
    let queue = runtime.queue
    let router = runtime.router
    content
      .alert(
        AlertText.serverProblemTitle(queue.serverProblem?.kind),
        isPresented: Binding(get: { queue.serverProblem != nil }, set: { if !$0 { runtime.dismissServerAlert() } })
      ) {
        Button("Open Settings") { runtime.openServerSettings() }
        Button("Try Again") { runtime.retryServer() }
        Button("Not Now", role: .cancel) { runtime.dismissServerAlert() }
      } message: {
        Text(queue.serverProblem.map(AlertText.serverProblemMessage) ?? "")
      }
      .alert(
        router.failureTitle, isPresented: Binding(get: { !router.failures.isEmpty }, set: { if !$0 { router.failures = [] } })
      ) {
        Button("OK", role: .cancel) { router.failures = [] }
      } message: {
        Text(router.failureMessage)
      }
      .alert(
        router.duplicate.map { AlertText.duplicateTitle($0, in: queue) } ?? "",
        isPresented: Binding(get: { router.duplicate != nil }, set: { if !$0 { router.dismissDuplicate() } }), presenting: router.duplicate
      ) { duplicate in
        Button("Download Again") { runtime.addAgain(duplicate) }
        // In the list (the unfinished copy, when there is one): select it.
        if let existing = duplicate.existingItemID, queue.item(existing) != nil {
          Button("Show in List") {
            router.dismissDuplicate()
            runtime.selection = existing
          }
        } else {
          Button("Show in Files") {
            router.dismissDuplicate()
            if let url = FilesLocation.filesAppURL(for: duplicate.folder) { openURL(url) }
          }
        }
        Button("Cancel", role: .cancel) { router.dismissDuplicate() }
      } message: { duplicate in
        Text(AlertText.duplicateMessage(duplicate, in: queue))
      }
      .alert("Download on Cellular?", isPresented: $gate.isAsking) {
        Button("Download") { runtime.gate.agree() }
        Button("Wait for Wi-Fi", role: .cancel) { runtime.gate.wait() }
      } message: {
        Text("Releases are often several gigabytes. Downloads start by themselves once you are on Wi-Fi.")
      }
      .alert(
        "Couldn’t Delete Files", isPresented: Binding(get: { queue.actionError != nil }, set: { if !$0 { queue.actionError = nil } })
      ) {
        Button("OK", role: .cancel) { queue.actionError = nil }
      } message: {
        Text(queue.actionError ?? "")
      }
  }
}
