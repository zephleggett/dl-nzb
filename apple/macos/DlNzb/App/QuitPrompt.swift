import DlNzbUI
import Foundation

/// The question asked when quitting would interrupt downloads, per the SPEC:
/// "Quit dl-nzb?" and how many will continue. Nothing is lost by quitting
/// (jobs stop where they can resume), so the wording reassures rather than
/// warns.
struct QuitPrompt: Equatable {
  let title: String
  let message: String
  let confirm: String
  let cancel: String

  /// Nil when nothing is unfinished: quit straight away.
  init?(unfinished: Int) {
    guard unfinished > 0 else { return nil }
    title = "Quit dl-nzb?"
    message = "\(Format.count(unfinished, "download", "downloads")) will continue next time you open dl-nzb."
    confirm = "Quit"
    cancel = "Cancel"
  }
}
