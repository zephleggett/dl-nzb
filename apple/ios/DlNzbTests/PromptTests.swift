import DlNzbKit
import Foundation
import Testing

@testable import DlNzbApp

@Suite("Row questions")
struct PromptTests {
  @Test("Swipe › Delete asks about that download and leaves the list alone until answered")
  func swipeDelete() {
    var prompts = ItemPromptState()
    let item = Items.make(1, .finished(JobSummary(outcome: .completed, outputDirectory: URL(filePath: "/tmp"))))
    prompts.ask(.delete, about: item.id)
    #expect(prompts.isAsking(.delete))
    #expect(prompts.itemID == item.id)

    // Another question's dialog closing does not take this one down.
    prompts.dismiss(.stop)
    #expect(prompts.isAsking(.delete))

    // Answered or dismissed: no question, and the dialog keeps its item while it animates away.
    prompts.dismiss(.delete)
    #expect(prompts.question == nil)
    #expect(prompts.itemID == item.id)
  }

  @Test("A second question replaces the first, for the download it is about")
  func secondQuestion() {
    var prompts = ItemPromptState()
    prompts.ask(.delete, about: Items.make(1, .queued).id)
    prompts.ask(.stop, about: Items.make(2, .queued).id)
    #expect(prompts.isAsking(.stop))
    #expect(!prompts.isAsking(.delete))
    #expect(prompts.itemID == Items.make(2, .queued).id)
  }

  @Test("Remove from List asks only when it would stop a download or leave its data behind (the Kit's rule, as the menus use it)")
  func removalConfirmation() {
    let downloading = Items.downloading(1, done: 1_000, of: 8_000)
    #expect(downloading.needsRemovalConfirmation)

    #expect(!Items.make(2, .queued).needsRemovalConfirmation)
    var paused = Items.make(3, .paused, progress: JobProgress(phase: .downloading, bytesDone: 500, bytesTotal: 8_000))
    #expect(paused.needsRemovalConfirmation)
    paused.progress = nil
    #expect(!paused.needsRemovalConfirmation)

    let stopped = Items.make(4, .stopped, progress: JobProgress(phase: .downloading, bytesDone: 500, bytesTotal: 8_000))
    #expect(stopped.needsRemovalConfirmation)

    let finished = Items.make(
      5, .finished(JobSummary(outcome: .completed, outputDirectory: URL(filePath: "/tmp"))),
      progress: JobProgress(phase: .downloading, bytesDone: 8_000, bytesTotal: 8_000))
    #expect(!finished.needsRemovalConfirmation)
  }
}
