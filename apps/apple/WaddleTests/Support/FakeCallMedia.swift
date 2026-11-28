import Observation
import SwiftUI
import WaddleKit
@testable import Waddle

@MainActor @Observable
final class FakeCallMedia: CallMediaControls {
    enum Call: Equatable {
        case video(String, CallVideoSource)
        case setVolume(Double, String), resetVolumes
        case setMicrophone(Bool), setCamera(Bool)
        case refreshDevices, chooseDevice(CallDeviceKind, String?)
        case setMeasuresVideo(Bool), videoSamples
        case setBackground(CallBackground), restartBackground
        case setAudioProcessing(CallAudioProcessing)
        case screenSources, shareScreen(CallScreenSource), stopScreenShare
    }

    private(set) var calls: [Call] = []
    var onCall: ((Call) -> Void)?

    var phase: CallMediaPhase = .connected
    var connectedSince: Date?
    var mediaUpSince: Date?
    var isMicrophoneOn = false
    var isCameraOn = false
    var captureIssues: [CallMediaIssue.Kind: CallMediaIssue.Reason] = [:]
    var participants: [CallMediaParticipant] = []
    var activeSpeakersLoudestFirst: [String] = []
    var dockCameraIdentities = CallDockCameraIdentities()
    var videoView: AnyView?
    var hasChangedVolumes = false
    var devices = CallDevices()
    var connectionQuality: CallConnectionQuality = .unknown
    var samples: [CallVideoSample] = []
    var mediaRefusals: [CallMediaIssue.Kind: Int] = [:]
    var background: CallBackground = .off
    var isBackgroundFailing = false
    var audioProcessing = CallAudioProcessing()
    var canShareScreen = true
    var isScreenShareOn = false
    var isScreenShareStarting = false
    var isScreenShareBlocked = false
    var didScreenShareFail = false
    var sources: CallScreenSources?

    func video(of identity: String, source: CallVideoSource) -> AnyView? {
        record(.video(identity, source))
        return videoView
    }

    func setVolume(_ gain: Double, of identity: String) { record(.setVolume(gain, identity)) }
    func resetVolumes() { record(.resetVolumes) }
    func setMicrophone(_ on: Bool) async {
        isMicrophoneOn = on
        record(.setMicrophone(on))
    }

    func setCamera(_ on: Bool) async {
        isCameraOn = on
        record(.setCamera(on))
    }
    func refreshDevices() async { record(.refreshDevices) }
    func chooseDevice(_ kind: CallDeviceKind, id: String?) async { record(.chooseDevice(kind, id)) }
    func setMeasuresVideo(_ on: Bool) async { record(.setMeasuresVideo(on)) }

    func videoSamples() async -> [CallVideoSample] {
        record(.videoSamples)
        return samples
    }

    func setBackground(_ background: CallBackground) { record(.setBackground(background)) }
    func restartBackground() async { record(.restartBackground) }
    func setAudioProcessing(_ processing: CallAudioProcessing) async { record(.setAudioProcessing(processing)) }

    func screenSources() async -> CallScreenSources? {
        record(.screenSources)
        return sources
    }

    func shareScreen(_ source: CallScreenSource) async { record(.shareScreen(source)) }
    func stopScreenShare() async { record(.stopScreenShare) }

    private func record(_ call: Call) {
        calls.append(call)
        onCall?(call)
    }
}
