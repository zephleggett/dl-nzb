import os

/// The app's loggers, one category per area, all under the bundle's subsystem
/// so Console and `log stream --predicate 'subsystem == "com.zephleggett.dl-nzb"'`
/// show everything dl-nzb says.
public enum Log {
  public static let subsystem = "com.zephleggett.dl-nzb"

  public static let app = Logger(subsystem: subsystem, category: "app")
  public static let queue = Logger(subsystem: subsystem, category: "queue")
  public static let settings = Logger(subsystem: subsystem, category: "settings")
  public static let engine = Logger(subsystem: subsystem, category: "engine")
  public static let persistence = Logger(subsystem: subsystem, category: "persistence")
}
