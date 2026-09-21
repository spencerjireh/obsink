import BackgroundTasks
import Foundation

/// `BGAppRefreshTask` driver: registered at launch, scheduled whenever the
/// app goes to the background, and runs the same auto-sync routine as a
/// foreground activation. Opportunistic by design; it turns "never until
/// the user opens the app" into "usually within the hour".
enum BackgroundRefresh {
    /// Must match `BGTaskSchedulerPermittedIdentifiers` in Info.plist.
    static let identifier = "com.obsink.ios.refresh"

    /// Call before the app finishes launching.
    static func register() {
        BGTaskScheduler.shared.register(forTaskWithIdentifier: identifier, using: nil) { task in
            guard let refresh = task as? BGAppRefreshTask else {
                task.setTaskCompleted(success: false)
                return
            }
            handle(refresh)
        }
    }

    static func schedule() {
        let request = BGAppRefreshTaskRequest(identifier: identifier)
        request.earliestBeginDate = Date(timeIntervalSinceNow: AutoSyncPolicy.staleAfter)
        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            // Simulator and some restricted contexts refuse; nothing to do.
            NSLog("ObSink: background refresh not scheduled: %@", "\(error)")
        }
    }

    static func handle(_ task: BGAppRefreshTask) {
        schedule()
        let completion = OnceCompletion(task)
        let work = Task { @MainActor in
            // A background launch may have no scene, so no model yet.
            let model = SyncModel.shared ?? SyncModel()
            model.cancelRequested = false
            await model.autoSync(reason: .background)
            completion.finish(success: !model.cancelRequested)
        }
        task.expirationHandler = {
            // The FFI sync in flight cannot be cancelled; the routine stops
            // before its next vault and the system is told we did not finish.
            Task { @MainActor in SyncModel.shared?.cancelRequested = true }
            work.cancel()
            completion.finish(success: false)
        }
    }

    /// `setTaskCompleted` must be called exactly once, from whichever of the
    /// routine and the expiration handler gets there first.
    private final class OnceCompletion: @unchecked Sendable {
        private let task: BGAppRefreshTask
        private let lock = NSLock()
        private var done = false

        init(_ task: BGAppRefreshTask) { self.task = task }

        func finish(success: Bool) {
            lock.lock()
            defer { lock.unlock() }
            guard !done else { return }
            done = true
            task.setTaskCompleted(success: success)
        }
    }
}
