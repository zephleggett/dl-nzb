import DlNzbKit
import DlNzbUI
import SwiftUI

/// Every question and problem the main window raises: a duplicate, a file
/// that could not be added, a server problem, a failed Move to Trash, and the
/// Stop, Remove from List and Move to Trash confirmations. Release names
/// here are plain: alerts wrap them themselves, and VoiceOver reads them.
struct QueueAlerts: ViewModifier {
  @Environment(MacApp.self) private var app
  @Environment(DownloadQueue.self) private var queue
  @Environment(\.openSettings) private var openSettings

  func body(content: Content) -> some View {
    content
      .alert(duplicateTitle, isPresented: presence(!app.duplicates.isEmpty), presenting: app.duplicates.first) { duplicate in
        Button("Download Again") { app.downloadAgain(duplicate) }
        Button(AlertText.duplicateShowTitle(duplicate, in: queue)) { app.dismissDuplicate(duplicate, revealing: true) }
        Button("Cancel", role: .cancel) { app.dismissDuplicate(duplicate, revealing: false) }
      } message: { duplicate in
        Text(AlertText.duplicateMessage(duplicate, in: queue))
      }
      .alert(problemTitle, isPresented: presence(!app.addProblems.isEmpty)) {
        Button("OK") { app.addProblems.removeAll() }
      } message: {
        Text(app.addProblems.map(\.message).joined(separator: "\n"))
      }
      .alert(
        AlertText.serverProblemTitle(queue.serverProblem?.kind), isPresented: presence(queue.serverProblem != nil), presenting: queue.serverProblem
      ) { _ in
        Button("Open Settings") {
          app.prepareServerSettings()
          openSettings()
        }
        Button("Try Again") { app.retryServer() }
        Button("Not Now", role: .cancel) { queue.dismissServerProblem() }
      } message: { problem in
        Text(AlertText.serverProblemMessage(problem))
      }
      .alert("Couldn’t Move to Trash", isPresented: presence(queue.actionError != nil), presenting: queue.actionError) { _ in
        Button("OK") { queue.actionError = nil }
      } message: { message in
        Text(message)
      }
      .confirmationDialog(stopTitle, isPresented: presence(app.stopRequest != nil), titleVisibility: .visible, presenting: app.stopRequest) {
        request in
        Button("Stop and Keep Data") { app.stop(request, deletingData: false) }
        Button("Stop and Delete Data", role: .destructive) { app.stop(request, deletingData: true) }
        Button("Cancel", role: .cancel) { app.stopRequest = nil }
      } message: { request in
        Text(
          request.isSingle
            ? "Keep what has downloaded so Retry can continue, or delete it." : "Keep what has downloaded so Retry can continue, or delete it all.")
      }
      .confirmationDialog(removeTitle, isPresented: presence(app.removeRequest != nil), titleVisibility: .visible, presenting: app.removeRequest) {
        request in
        Button("Remove and Keep Data") { app.remove(request, deletingData: false) }
        Button("Remove and Delete Data", role: .destructive) { app.remove(request, deletingData: true) }
        Button("Cancel", role: .cancel) { app.removeRequest = nil }
      } message: { request in
        Text(removeMessage(request))
      }
      .confirmationDialog(trashTitle, isPresented: presence(app.trashRequest != nil), titleVisibility: .visible, presenting: app.trashRequest) {
        request in
        Button("Move to Trash", role: .destructive) { app.moveToTrash(request) }
        Button("Cancel", role: .cancel) { app.trashRequest = nil }
      } message: { request in
        Text(request.isSingle ? "Its folder and everything in it go to the Trash." : "Their folders and everything in them go to the Trash.")
      }
  }

  /// Shown while `condition` holds. Each alert's buttons clear the state
  /// themselves, so a dismissal through the binding has nothing left to do.
  private func presence(_ condition: Bool) -> Binding<Bool> {
    Binding(get: { condition }, set: { _ in })
  }

  // MARK: Copy

  private var duplicateTitle: String {
    app.duplicates.first.map { AlertText.duplicateTitle($0, in: queue) } ?? ""
  }

  private var problemTitle: String {
    let problems = app.addProblems
    return problems.count == 1 ? "Couldn’t Open “\(problems[0].fileName)”" : "Couldn’t Open \(problems.count.formatted()) Files"
  }

  private var stopTitle: String {
    guard let request = app.stopRequest else { return "" }
    return request.isSingle ? "Stop “\(ReleaseText.spoken(request.name))”?" : "Stop \(request.name)?"
  }

  private var removeTitle: String {
    guard let request = app.removeRequest else { return "" }
    return request.isSingle ? "Remove “\(ReleaseText.spoken(request.name))” from the List?" : "Remove \(request.name) from the List?"
  }

  /// What removing does: running downloads stop, and what has downloaded
  /// can stay in its folder or go. Finished downloads keep their files.
  private func removeMessage(_ request: ItemRequest) -> String {
    if request.isSingle {
      return request.hasRunning
        ? "The download stops. Keep what it has downloaded, or delete it." : "Keep what it has downloaded in its folder, or delete it."
    }
    let running = request.hasRunning ? "Running downloads stop. " : ""
    let finished = request.hasFinished ? " Finished downloads keep their files." : ""
    return "\(running)Keep what they have downloaded, or delete it.\(finished)"
  }

  private var trashTitle: String {
    guard let request = app.trashRequest else { return "" }
    return request.isSingle ? "Move “\(ReleaseText.spoken(request.name))” to the Trash?" : "Move \(request.name) to the Trash?"
  }
}
