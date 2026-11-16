import SwiftUI
import WaddleKit

/// What the call dock reads from, and asks of, the media session.
@MainActor
protocol CallMediaControls: AnyObject, Observable {
    var phase: CallMediaPhase { get }
    /// When this call's media first connected; a reconnect keeps it.
    var connectedSince: Date? { get }
    /// Actually publishing, not merely requested.
    var isMicrophoneOn: Bool { get }
    var isCameraOn: Bool { get }
    /// The person has refused Waddle this device, so its control cannot turn it on.
    var isMicrophoneBlocked: Bool { get }
    var isCameraBlocked: Bool { get }
    /// The peer's camera, while subscribed and unmuted.
    var remoteVideo: AnyView? { get }
    /// Our camera preview.
    var localVideo: AnyView? { get }
    /// The SFU's participant identities (full JIDs), ours first, while
    /// connected; empty otherwise, including while reconnecting.
    var participantIdentities: [String] { get }
    /// The identities the SFU hears speaking now, ours included.
    var speakingIdentities: Set<String> { get }
    func setMicrophone(_ on: Bool) async
    func setCamera(_ on: Bool) async
}

extension EnvironmentValues {
    /// The signed-in session's media controls; `nil` outside a session.
    @Entry var callMediaControls: (any CallMediaControls)?
}
