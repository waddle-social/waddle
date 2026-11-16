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

    /// The microphone starts a moment after the media connects. For this
    /// long after connecting, a state must hold before the room hears it,
    /// so joining is not broadcast as a mute and then an unmute. After
    /// that a change is the person's own and goes out at once.
    static let settle: TimeInterval = 1

    func body(content: Content) -> some View {
        content.task(id: marker) {
            guard let marker, marker.isMuted != calls.isSelfMuteAdvertised else { return }
            let sinceConnect = media.connectedSince.map { Date.now.timeIntervalSince($0) } ?? Self.settle
            if sinceConnect < Self.settle {
                try? await Task.sleep(for: .seconds(Self.settle - sinceConnect))
            }
            guard !Task.isCancelled, case .inGroup(let call) = calls.state, call.sid == marker.sid,
                marker.isMuted != calls.isSelfMuteAdvertised else { return }
            _ = await calls.advertiseSelfMute(marker.isMuted)
        }
    }

    /// Only a room's call the mixer accepted, with its media connected, has
    /// a microphone to report on.
    private var marker: Marker? {
        guard case .inGroup(let call) = calls.state, media.phase == .connected else { return nil }
        return Marker(sid: call.sid, isMuted: !media.isMicrophoneOn)
    }

    private struct Marker: Hashable {
        let sid: String
        let isMuted: Bool
    }
}

extension View {
    /// Keeps the room's mute marker in step with the microphone (see `SelfMuteMarker`).
    func advertisingSelfMute(calls: CallStore, media: any CallMediaControls) -> some View {
        modifier(SelfMuteMarker(calls: calls, media: media))
    }
}
