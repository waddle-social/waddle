import SwiftUI
import WaddleKit

struct CallMediaParticipant: Identifiable, Equatable {
    /// The full JID the SFU knows this device by.
    let identity: String
    let isLocal: Bool
    let isCameraOn: Bool
    /// The publication of the screen this device is sharing, while we can see it.
    var screenPublicationID: String?
    let hasMicrophoneTrack: Bool
    let isSpeaking: Bool
    /// Playout gain from 0 to 2, with 1 at unity.
    let volume: Double

    var id: String { identity }
}

struct CallDockCameraIdentities: Equatable {
    var local: String?
    var remote: String?
    /// Someone sharing their screen: the dock shows it in place of a camera.
    var remoteScreen: String?
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
    /// This device can share its screen into a call: a Mac.
    var canShareScreen: Bool { get }
    /// Our screen, or a window of ours, is being shared. Its picture is `video(of:source:)` of our own identity.
    var isScreenShareOn: Bool { get }
    /// The person has refused Waddle the screen, so nothing can be listed or shared.
    var isScreenShareBlocked: Bool { get }
    /// What can be shared now, asking for the screen if nobody has yet; `nil` when it is refused.
    func screenSources() async -> CallScreenSources?
    /// Shares `source`, in place of whatever was shared before.
    func shareScreen(_ source: CallScreenSource) async
    func stopScreenShare() async
}

extension EnvironmentValues {
    /// The signed-in session's media controls; `nil` outside a session.
    @Entry var callMediaControls: (any CallMediaControls)?
}
