import SwiftUI
import WaddleKit

struct CallMediaParticipant: Identifiable, Equatable {
    /// The full JID the SFU knows this device by.
    let identity: String
    let isLocal: Bool
    let isCameraOn: Bool
    let hasMicrophoneTrack: Bool
    let isSpeaking: Bool
    /// Playout gain from 0 to 2, with 1 at unity.
    let volume: Double

    var id: String { identity }
}

struct CallDockCameraIdentities: Equatable {
    var local: String?
    var remote: String?
}

/// What call views read from, and ask of, the media session.
@MainActor
protocol CallMediaControls: AnyObject, Observable {
    var phase: CallMediaPhase { get }
    /// When this call's media first connected; a reconnect keeps it.
    var connectedSince: Date? { get }
    /// When media last came up: the first connect, or the end of a reconnect.
    /// The microphone and camera start a moment after it.
    var mediaUpSince: Date? { get }
    /// Actually publishing, not merely requested.
    var isMicrophoneOn: Bool { get }
    var isCameraOn: Bool { get }
    /// The person has refused Waddle this device, so its control cannot turn it on.
    var isMicrophoneBlocked: Bool { get }
    var isCameraBlocked: Bool { get }
    /// Ours first, then join order. Empty until connected and during reconnects.
    var participants: [CallMediaParticipant] { get }
    /// For speaker promotion. Updates only when the active identities or their order change.
    var activeSpeakersLoudestFirst: [String] { get }
    /// The dock keeps its camera selection even when the roster is unavailable.
    var dockCameraIdentities: CallDockCameraIdentities { get }
    /// A camera is available only while its track is subscribed and unmuted.
    func video(of identity: String, source: CallVideoSource) -> AnyView?
    func setVolume(_ gain: Double, of identity: String)
    /// Someone plays at other than their own volume, whether or not they are in the call right now.
    var hasChangedVolumes: Bool { get }
    /// Everyone back to their own volume, a device that has left and may return included.
    func resetVolumes()
    func setMicrophone(_ on: Bool) async
    func setCamera(_ on: Bool) async
}

extension EnvironmentValues {
    /// The signed-in session's media controls; `nil` outside a session.
    @Entry var callMediaControls: (any CallMediaControls)?
}
