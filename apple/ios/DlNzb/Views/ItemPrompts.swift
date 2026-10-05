import DlNzbKit
import DlNzbUI
import SwiftUI

/// The questions an action on one download can raise: stop with data (keep
/// it or delete it), remove a download that is running or has data, delete
/// the files, and the archive's password.
///
/// One state per screen, keyed by the download, not one per row: a row's
/// swipe closes and rows move as the queue changes, and the question must
/// outlive both. The dialog itself is shown from the row it is about, which
/// iOS points it at.
struct ItemPromptState: Equatable {
  enum Question: Equatable {
    case stop
    case remove
    case delete
    case password
  }

  private(set) var question: Question?
  /// The download asked about. Kept after the answer, so the dialog keeps
  /// its words while it animates away.
  private(set) var itemID: DownloadItem.ID?

  mutating func ask(_ question: Question, about id: DownloadItem.ID) {
    itemID = id
    self.question = question
  }

  /// The question was answered or dismissed.
  mutating func dismiss(_ question: Question) {
    if self.question == question { self.question = nil }
  }

  func isAsking(_ question: Question) -> Bool {
    self.question == question
  }
}

extension View {
  /// The dialogs and the password alert for `state`. With `id`, only the
  /// questions about that download: a list attaches them to each row.
  func itemPrompts(state: Binding<ItemPromptState>, about id: DownloadItem.ID? = nil) -> some View {
    modifier(ItemPrompts(state: state, anchor: id))
  }
}

private struct ItemPrompts: ViewModifier {
  @Binding var state: ItemPromptState
  let anchor: DownloadItem.ID?
  @Environment(AppRuntime.self) private var runtime
  @State private var password = ""

  private var item: DownloadItem? {
    state.itemID.flatMap { runtime.queue.item($0) }
  }

  private var isMine: Bool {
    anchor == nil || state.itemID == anchor
  }

  private func isPresented(_ question: ItemPromptState.Question) -> Binding<Bool> {
    Binding(
      get: { isMine && state.isAsking(question) },
      set: { if !$0, isMine { state.dismiss(question) } })
  }

  /// The prompt asks again after a wrong password, saying so in the row's words.
  private var passwordMessage: String {
    let ask = "Enter the password for this download’s archive."
    return item?.passwordRejected == true ? "\(StatusText.passwordRejected). \(ask)" : ask
  }

  func body(content: Content) -> some View {
    let id = state.itemID
    content
      // Titles leave the release name out: a dotted name broken over four
      // lines reads worse than no name at all.
      .confirmationDialog("Stop This Download?", isPresented: isPresented(.stop), titleVisibility: .visible, presenting: id) { id in
        Button("Stop and Keep Data") { runtime.stop(id, deletingData: false) }
        Button("Stop and Delete Data", role: .destructive) { runtime.stop(id, deletingData: true) }
        Button("Cancel", role: .cancel) {}
      } message: { _ in
        Text("With its data kept, Retry continues where it stopped.")
      }
      .confirmationDialog("Remove This Download?", isPresented: isPresented(.remove), titleVisibility: .visible, presenting: id) { id in
        Button("Remove and Keep Data") { runtime.remove(id) }
        Button("Remove and Delete Data", role: .destructive) { runtime.remove(id, deletingData: true) }
        Button("Cancel", role: .cancel) {}
      } message: { _ in
        Text("Kept data stays in its folder in Files.")
      }
      .confirmationDialog("Delete This Download?", isPresented: isPresented(.delete), titleVisibility: .visible, presenting: id) { id in
        Button("Delete Files", role: .destructive) { runtime.deleteFiles(id) }
        Button("Cancel", role: .cancel) {}
      } message: { _ in
        Text("Its folder and everything in it are deleted from this \(UIDevice.current.model).")
      }
      .alert("Password Required", isPresented: isPresented(.password), presenting: id) { id in
        SecureField("Password", text: $password)
        Button("Cancel", role: .cancel) { password = "" }
        Button("Extract") {
          runtime.providePassword(id, password: password)
          password = ""
        }
        .disabled(password.isEmpty)
      } message: { _ in
        Text(passwordMessage)
      }
  }
}

// Both read the item as it is when the action runs, not as a menu or a
// swipe saw it: a download can start between the two.
extension AppRuntime {
  /// Stop, asking first when there is data to keep or delete.
  func requestStop(_ item: DownloadItem, prompts: inout ItemPromptState) {
    guard let item = queue.item(item.id) else { return }
    if item.hasData {
      prompts.ask(.stop, about: item.id)
    } else {
      stop(item.id, deletingData: false)
    }
  }

  /// Remove from List, asking first when that would stop a download or
  /// leave its data behind.
  func requestRemove(_ item: DownloadItem, prompts: inout ItemPromptState) {
    guard let item = queue.item(item.id) else { return }
    if item.needsRemovalConfirmation {
      prompts.ask(.remove, about: item.id)
    } else {
      remove(item.id)
    }
  }
}
