import DlNzbKit

extension AppModel.EngineKind {
  /// The engine `AppModel` asks for: the Rust one, or the simulated one when
  /// launched with `-simulate YES`.
  public func makeEngine() -> any DownloadEngine {
    switch self {
    case .rust: RustEngine()
    case .simulated: SimulatedEngine()
    }
  }
}
