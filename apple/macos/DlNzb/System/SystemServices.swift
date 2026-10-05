import DlNzbKit
import Foundation

/// The Mac's system integration, kept in step with the queue: the Dock tile,
/// Finder's progress on job folders, the sleep and App Nap activity, and
/// notifications.
@MainActor
final class SystemServices {
  let dockTile = DockTileProgress()
  let finder = FinderProgress()
  let sleep = SleepGuard()
  let notifier = Notifier()
  private var watcher: Watcher?

  func start(app: MacApp) {
    let queue = app.queue
    let settings = app.settings
    notifier.install()
    notifier.onReveal = { [weak app] id in app?.reveal([id]) }
    notifier.onOpen = { [weak app] id in
      app?.showMainWindow()
      app?.selection = [id]
    }
    queue.onItemFinished = { [notifier, settings] item in
      guard settings.notifyWhenFinished else { return }
      notifier.notify(item)
    }
    finder.onCancel = { [weak queue] id in queue?.stop(id) }
    watcher = Watcher { [weak self, weak queue, weak settings] in
      guard let self, let queue, let settings else { return }
      self.dockTile.update(fraction: queue.overallFraction, unfinished: queue.unfinishedCount)
      self.finder.update(items: queue.items)
      self.sleep.update(active: queue.activeCount > 0, preventSleep: settings.preventSleep)
    }
  }

  /// Quitting: the Dock tile back to the icon, Finder's bars gone, the
  /// activity ended.
  func stop() {
    watcher?.stop()
    watcher = nil
    dockTile.reset()
    finder.unpublishAll()
    sleep.end()
  }
}
