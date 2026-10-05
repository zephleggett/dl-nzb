import DlNzbKit
import DlNzbRust

/// The engine `AppModel` asks for. `-simulate YES` asks for `.simulated`;
/// everything else for `.rust`.
enum Engines {
  static func make(_ kind: AppModel.EngineKind) -> any DownloadEngine {
    switch kind {
    case .rust: RustEngine()
    case .simulated: SimulatedEngine()
    }
  }
}
