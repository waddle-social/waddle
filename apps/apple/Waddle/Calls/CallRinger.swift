import Foundation
import WaddleKit
#if os(macOS)
import AppKit
#endif

/// Makes a call heard and seen outside the dock: the ring and ringback
/// tones, and a notification with Accept and Decline while Waddle is not
/// in front. Driven by the session's `CallAlert`, so everything it started
/// ends the moment the call is answered, declined or gone. `CallRingPlan`
/// decides what an alert needs; this carries it out.
@MainActor
final class CallRinger {
    /// An alert with what its notification shows.
    struct Request: Equatable {
        let alert: CallAlert
        let account: BareJID
        let callerName: String
    }

    private let notifications: NotificationController
    private let tones = CallTonePlayer()
    private var request: Request?
    private var notifiedSid: String?
    #if os(macOS)
    private var activity: [NSObjectProtocol] = []
    #endif

    /// Whether Waddle is in front. A ring that starts in front and is then
    /// left behind still gets its notification.
    ///
    /// On an iPhone or iPad the scene says so. On a Mac the application
    /// does, and only with a window to show the call in: its windows can
    /// be closed, minimised or behind another app's while it still runs.
    var isAppActive = true {
        didSet { if isAppActive != oldValue { refresh() } }
    }

    init(notifications: NotificationController) {
        self.notifications = notifications
        #if os(macOS)
        isAppActive = Self.isInFront
        // Read afresh on each change, so the order these arrive in cannot
        // leave a stale answer behind.
        let changes = [
            NSApplication.didBecomeActiveNotification, NSApplication.didResignActiveNotification,
            NSWindow.didMiniaturizeNotification, NSWindow.didDeminiaturizeNotification,
            NSWindow.didBecomeMainNotification, NSWindow.willCloseNotification,
        ]
        for name in changes {
            activity.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                // After this turn: a closing window still counts as visible
                // while its notification is being delivered.
                Task { @MainActor in self?.isAppActive = Self.isInFront }
            })
        }
        #endif
    }

    #if os(macOS)
    private static var isInFront: Bool {
        NSApplication.shared.isActive
            && NSApplication.shared.windows.contains { $0.isVisible && !$0.isMiniaturized && $0.canBecomeMain }
    }
    #endif

    /// What the session's call slot asks for now; nil for silence.
    func show(_ next: Request?) {
        guard request != next else { return }
        request = next
        #if os(macOS)
        // A window can go without a notification this listens for.
        isAppActive = Self.isInFront
        #endif
        refresh()
    }

    private func refresh() {
        let plan = CallRingPlan(alert: request?.alert, isAppActive: isAppActive, ringsInBackground: Self.ringsInBackground)
        if let tone = plan.tone { tones.play(tone) } else { tones.stop() }
        var ring: CallAlert.IncomingRing?
        if plan.notifies, case .ring(let incoming) = request?.alert { ring = incoming }
        if let notifiedSid, notifiedSid != ring?.sid {
            notifications.clearIncomingCall(sid: notifiedSid)
            self.notifiedSid = nil
        }
        if let ring, let request, notifiedSid == nil {
            notifications.postIncomingCall(sid: ring.sid, callerName: request.callerName, isVideo: ring.isVideo, for: request.account)
            notifiedSid = ring.sid
        }
    }

    private static var ringsInBackground: Bool {
        #if os(macOS)
        true
        #else
        false
        #endif
    }
}
