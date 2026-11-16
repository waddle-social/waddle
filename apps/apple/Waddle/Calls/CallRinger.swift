import Foundation
import WaddleKit

/// Makes a call heard and seen outside the dock: the ring and ringback
/// tones, and a notification with Accept and Decline while Waddle is not
/// in front. Driven by the session's `CallAlert`, so everything it started
/// ends the moment the call is answered, declined or gone.
@MainActor
final class CallRinger {
    /// A ring with what the notification shows.
    struct Ring: Equatable {
        let alert: CallAlert.IncomingRing
        let account: BareJID
        let callerName: String
    }

    private enum Showing: Equatable {
        case ring(Ring)
        case ringback
    }

    private let notifications: NotificationController
    private let tones = CallTonePlayer()
    private var showing: Showing?
    private var notifiedSid: String?

    /// Whether Waddle is frontmost. A ring that starts in front and is then
    /// left behind still gets its notification.
    var isAppActive = true {
        didSet { if isAppActive != oldValue { refresh() } }
    }

    init(notifications: NotificationController) {
        self.notifications = notifications
    }

    func ring(_ ring: Ring) { show(.ring(ring)) }
    func ringback() { show(.ringback) }
    func stop() { show(nil) }

    private func show(_ next: Showing?) {
        guard showing != next else { return }
        showing = next
        refresh()
    }

    private func refresh() {
        var ringToNotify: Ring?
        switch showing {
        case .ring(let ring):
            if !isAppActive { ringToNotify = ring }
            // An iPhone or iPad in the background rings through its
            // notification's sound; an app there may not start audio.
            if Self.playsRingInBackground || isAppActive { tones.play(.ring) } else { tones.stop() }
        case .ringback:
            tones.play(.ringback)
        case nil:
            tones.stop()
        }
        if let notifiedSid, notifiedSid != ringToNotify?.alert.sid {
            notifications.clearIncomingCall(sid: notifiedSid)
            self.notifiedSid = nil
        }
        if let ring = ringToNotify, notifiedSid == nil {
            notifications.postIncomingCall(
                sid: ring.alert.sid, callerName: ring.callerName, isVideo: ring.alert.isVideo,
                for: ring.account, playsSound: !Self.playsRingInBackground)
            notifiedSid = ring.alert.sid
        }
    }

    private static var playsRingInBackground: Bool {
        #if os(macOS)
        true
        #else
        false
        #endif
    }
}
