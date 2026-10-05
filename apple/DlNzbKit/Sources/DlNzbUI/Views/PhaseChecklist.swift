import DlNzbKit
import SwiftUI

/// Check · Download · Verify · Repair · Extract, each done, running, skipped
/// or stuck: the inspector's (and the iPhone detail screen's) account of what
/// a job has been through. A plain list in the inspector's own material.
public struct PhaseChecklist: View {
  let steps: [PhaseStep]

  public init(_ item: DownloadItem) {
    self.steps = PhaseStep.steps(for: item)
  }

  public init(steps: [PhaseStep]) {
    self.steps = steps
  }

  public var body: some View {
    VStack(alignment: .leading, spacing: 8) {
      ForEach(steps) { step in
        HStack(spacing: 8) {
          PhaseStepGlyph(status: step.status)
            .frame(width: 18, height: 18)
          Text(step.kind.title)
            .foregroundStyle(step.status == .pending || step.status == .skipped ? .secondary : .primary)
          Spacer(minLength: 8)
          // No rolling digits: the detail changes four times a second.
          if let detail = detail(for: step) {
            Text(detail)
              .font(.callout)
              .foregroundStyle(.secondary)
              .monospacedDigit()
          }
        }
        .accessibilityElement(children: .combine)
        .accessibilityValue(accessibilityValue(for: step))
      }
    }
  }

  private func detail(for step: PhaseStep) -> String? {
    if let detail = step.detail { return detail }
    switch step.status {
    case .skipped: return "Skipped"
    case .interrupted(let fraction?), .stopped(let fraction?): return Format.percent(fraction)
    default: return nil
    }
  }

  private func accessibilityValue(for step: PhaseStep) -> String {
    switch step.status {
    case .pending: "Not started"
    case .active: "In progress"
    case .interrupted: "Paused"
    case .stopped: "Stopped"
    case .done: "Done"
    case .skipped: "Skipped"
    case .attention: "Needs attention"
    case .failed: "Failed"
    }
  }
}

/// ✓ for done, a small determinate ring while running, a dash for skipped.
struct PhaseStepGlyph: View {
  let status: PhaseStep.Status

  var body: some View {
    switch status {
    case .pending:
      Image(systemName: "circle")
        .foregroundStyle(.tertiary)
    case .active(let fraction):
      ProgressRing(fraction: fraction, glyph: .none, lineWidth: 2)
        .scaleEffect(18 / 28)
    case .interrupted:
      Image(systemName: "pause.circle")
        .foregroundStyle(.secondary)
    case .stopped:
      Image(systemName: "stop.circle")
        .foregroundStyle(.secondary)
    case .done:
      Image(systemName: "checkmark.circle.fill")
        .symbolRenderingMode(.palette)
        .foregroundStyle(.white, .green)
    case .skipped:
      Image(systemName: "minus.circle")
        .foregroundStyle(.tertiary)
    case .attention:
      Image(systemName: "exclamationmark.circle.fill")
        .symbolRenderingMode(.palette)
        .foregroundStyle(.white, .orange)
    case .failed:
      Image(systemName: "xmark.circle.fill")
        .symbolRenderingMode(.palette)
        .foregroundStyle(.white, .red)
    }
  }
}

#Preview("Repairing") {
  PhaseChecklist(PreviewData.repairing)
    .padding()
    .frame(width: 280)
}

#Preview("Finished with a repair") {
  PhaseChecklist(PreviewData.finished)
    .padding()
    .frame(width: 280)
}

#Preview("Needs a password") {
  PhaseChecklist(PreviewData.needsPassword)
    .padding()
    .frame(width: 280)
}

#Preview("Failed") {
  PhaseChecklist(PreviewData.failed)
    .padding()
    .frame(width: 280)
}
