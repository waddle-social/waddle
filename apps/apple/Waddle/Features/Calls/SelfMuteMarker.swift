import SwiftUI
import WaddleKit

/// Tells a room's call whether our microphone is publishing, so everyone's
/// roster shows us muted when it is not, whatever turned it off: the mute
/// control, a refused permission, a missing device.
///
/// Applied once per session, never per dock: a sheet shows a second dock.
private struct SelfMuteMarker: ViewModifier {
    let calls: CallStore
    let media: any CallMediaControls
    /// The room can be reached; coming back online is a reason to try again.
    let isOnline: Bool

    /// The microphone starts a moment after media comes up, at the first
    /// connect and again after a reconnect republishes it. For this long
    /// after that, a state must hold before the room hears it, so neither
    /// is broadcast as a mute and then an unmute. Later a change is the
    /// person's own and goes out at once.
    static let settle: TimeInterval = 1
    /// A send that fails is tried again a few times; the room must not be
    /// left showing a mute state that is no longer true.
    static let retries = 3
    static let retryDelay: Duration = .seconds(2)

    func body(content: Content) -> some View {
        content.task(id: marker) {
            guard let marker, marker.isMuted != calls.confirmedSelfMute else { return }
            let sinceUp = media.mediaUpSince.map { Date.now.timeIntervalSince($0) } ?? Self.settle
            if sinceUp < Self.settle {
                try? await Task.sleep(for: .seconds(Self.settle - sinceUp))
            }
            for _ in 0...Self.retries {
                // A newer state cancels this task; a moved slot ends it.
                guard !Task.isCancelled, case .inGroup(let call) = calls.state, call.sid == marker.sid else { return }
                // Compared with what the room heard, not with what we meant
                // to say: a send that failed is still owed.
                if marker.isMuted == calls.confirmedSelfMute { return }
                if await calls.advertiseSelfMute(marker.isMuted) { return }
                try? await Task.sleep(for: Self.retryDelay)
            }
        }
    }

    /// Only a room's call the mixer accepted, with its media connected, has
    /// a microphone to report on.
    private var marker: Marker? {
        guard isOnline, case .inGroup(let call) = calls.state, media.phase == .connected else { return nil }
        return Marker(sid: call.sid, isMuted: !media.isMicrophoneOn)
    }

    private struct Marker: Hashable {
        let sid: String
        let isMuted: Bool
    }
}

extension View {
    /// Keeps the room's mute marker in step with the microphone (see `SelfMuteMarker`).
    func advertisingSelfMute(calls: CallStore, media: any CallMediaControls, isOnline: Bool) -> some View {
        modifier(SelfMuteMarker(calls: calls, media: media, isOnline: isOnline))
    }
}
