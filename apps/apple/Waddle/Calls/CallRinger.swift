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
    /// does: its windows can be closed, or behind another app's, while it
    /// still runs.
    var isAppActive = true {
        didSet { if isAppActive != oldValue { refresh() } }
    }

    init(notifications: NotificationController) {
        self.notifications = notifications
        #if os(macOS)
        isAppActive = NSApplication.shared.isActive
        let center = NotificationCenter.default
        for (name, active) in [(NSApplication.didBecomeActiveNotification, true), (NSApplication.didResignActiveNotification, false)] {
            activity.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                Task { @MainActor in self?.isAppActive = active }
            })
        }
        #endif
    }

    /// What the session's call slot asks for now; nil for silence.
    func show(_ next: Request?) {
        guard request != next else { return }
        request = next
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
