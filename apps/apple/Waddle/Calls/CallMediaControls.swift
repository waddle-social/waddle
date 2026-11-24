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
    /// The microphones, speakers and cameras there are, and which the person chose.
    /// Empty for a kind this device does not let an app choose, as audio on iOS.
    var devices: CallDevices { get }
    /// Reads the devices again; they are also re-read when one is plugged in or removed.
    func refreshDevices() async
    /// Uses device `id` for calls from now on, this one included; `nil` goes back to the system's choice.
    func chooseDevice(_ kind: CallDeviceKind, id: String?) async
    /// How good our own connection to the call is; unknown until the server has measured it.
    var connectionQuality: CallConnectionQuality { get }
    /// One more, or one fewer, is looking at the call's video figures. The media engine measures
    /// the streams while anyone is; measuring costs a little, so it stops with the last of them.
    func setMeasuresVideo(_ on: Bool) async
    /// The latest reading of each video stream we send or receive; empty unless measuring.
    /// A stream that appeared since the last reading is measured from this one on.
    func videoSamples() async -> [CallVideoSample]
    /// How many times each of our devices has been tried and refused or failed in this call.
    var mediaRefusals: [CallMediaIssue.Kind: Int] { get }
    /// What stands behind us in our camera's picture.
    var background: CallBackground { get }
    /// Uses `background` for calls from now on, this one included.
    func setBackground(_ background: CallBackground)
    /// The blur is on and is not producing a picture, so nothing of the camera is sent.
    var isBackgroundFailing: Bool { get }
    /// Starts the blur afresh, as when someone asks to check again.
    func restartBackground() async
    /// What is done to the microphone's sound before it is sent.
    var audioProcessing: CallAudioProcessing { get }
    /// Uses `processing` for calls from now on, this one included.
    func setAudioProcessing(_ processing: CallAudioProcessing) async
    /// This device can share its screen into a call: a Mac.
    var canShareScreen: Bool { get }
    /// Our screen, or a window of ours, is being shared. Its picture is `video(of:source:)` of our own identity.
    var isScreenShareOn: Bool { get }
    /// A pick is on its way to being shared; asking to stop reaches it.
    var isScreenShareStarting: Bool { get }
    /// The person has refused Waddle the screen, so nothing can be listed or shared.
    var isScreenShareBlocked: Bool { get }
    /// The last pick could not be shared: its window closed, or the call would not take it.
    var didScreenShareFail: Bool { get }
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
