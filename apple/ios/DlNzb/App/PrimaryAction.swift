import DlNzbKit
import DlNzbUI

/// What moves a download on, as its swipe, its ring and its detail screen's
/// main button offer it. Each lays it out its own way.
enum PrimaryAction: Equatable {
  /// A waiting download the queue holds back, which reads as paused
  /// (`AppRuntime.resumeHeld`).
  case resumeHeld
  /// Downloads wait for Start.
  case start
  case resume
  case pause
  /// Retry, or Download Again when it would start over.
  case retry(title: String)

  var title: String {
    switch self {
    case .resumeHeld, .resume: "Resume"
    case .start: "Start"
    case .pause: "Pause"
    case .retry(let title): title
    }
  }

  var systemImage: String {
    switch self {
    case .resumeHeld, .resume: "play.fill"
    case .start: "arrow.down"
    case .pause: "pause.fill"
    case .retry: "arrow.clockwise"
    }
  }
}

extension AppRuntime {
  /// The action for the item as the queue stands, first that applies:
  /// resume what the queue holds back, start what waits for Start, resume
  /// what is paused, pause what can be, retry what failed or stopped. Nil
  /// while nothing applies (post-processing, a question, finished).
  func primaryAction(for item: DownloadItem) -> PrimaryAction? {
    if queue.isHeld(item) { return .resumeHeld }
    if queue.awaitsStart(item) { return .start }
    if item.canResume { return .resume }
    if item.canPause { return .pause }
    if item.canRetry { return .retry(title: StatusText.retryTitle(for: item)) }
    return nil
  }

  func perform(_ action: PrimaryAction, on id: DownloadItem.ID) {
    switch action {
    case .resumeHeld: resumeHeld(id)
    case .start: start(id)
    case .resume: resume(id)
    case .pause: pause(id)
    case .retry: retry(id)
    }
  }
}
