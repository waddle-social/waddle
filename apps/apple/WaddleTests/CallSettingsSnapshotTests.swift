import SwiftUI
import WaddleKit
import XCTest

@testable import Waddle

/// The call settings sheet. Drawn in a window: its pickers are system controls.
@MainActor
final class CallSettingsSnapshotTests: XCTestCase {
    private static let builtIn = CallDevice(id: "builtin-mic", name: "MacBook Pro Microphone")
    private static let headset = CallDevice(id: "usb-1", name: "Jabra Evolve2 65")
    private static let speakers = CallDevice(id: "builtin-out", name: "MacBook Pro Speakers")
    private static let faceTime = CallDevice(id: "cam-1", name: "FaceTime HD Camera")

    private func assertSettings(
        _ devices: CallDevices, kinds: [CallDeviceKind] = CallDeviceKind.allCases, width: CGFloat = CallSettingsView.width,
        audioProcessing: CallAudioProcessing = CallAudioProcessing(), videoStats: [CallVideoStat]? = nil,
        height: CGFloat = 520, testName: String = #function, line: UInt = #line
    ) async throws {
        try await assertScreenSnapshot(
            of: CallSettingsView(
                devices: devices, kinds: kinds, audioProcessing: audioProcessing, videoStats: videoStats, choose: { _, _ in /* drawn only */ }, done: { /* drawn only */ }
            ),
            size: CGSize(width: width, height: height), testName: testName, line: line
        )
    }

    func testAChosenMicrophoneAndTheSystemsSpeakerAndCamera() async throws {
        try await assertSettings(CallDevices(
            microphones: CallDeviceList(devices: [Self.builtIn, Self.headset], preferred: Self.headset.id),
            speakers: CallDeviceList(devices: [Self.speakers]),
            cameras: CallDeviceList(devices: [Self.faceTime])
        ))
    }

    /// The chosen headset is unplugged and no camera is found.
    func testAChosenDeviceThatIsGoneAndNoCamera() async throws {
        try await assertSettings(
            CallDevices(
                microphones: CallDeviceList(devices: [Self.builtIn], preferred: Self.headset.id),
                speakers: CallDeviceList(devices: [Self.speakers])
            ),
            audioProcessing: CallAudioProcessing(noiseSuppression: false, autoGainControl: false), height: 560
        )
    }

    private static let devices = CallDevices(
        microphones: CallDeviceList(devices: [builtIn]), speakers: CallDeviceList(devices: [speakers]),
        cameras: CallDeviceList(devices: [faceTime])
    )

    /// Our camera going out and someone's screen coming in, as the tracker writes them.
    func testVideoStatistics() async throws {
        var tracker = CallVideoStatsTracker()
        func readings(second: Double) -> [CallVideoSample] {
            [
                CallVideoSample(
                    key: "me:camera:sent", label: "You", source: .camera, direction: .sent, width: 1280, height: 720,
                    framesPerSecond: 30, bytes: UInt64(150_000 * second), timestamp: 1_700_000_000_000 + second * 1000,
                    lossFraction: 0.004, roundTrip: 0.038, codec: "video/VP9", candidateType: "srflx", transport: "udp"
                ),
                CallVideoSample(
                    key: "linus:screen:received", label: "linus", source: .screen, direction: .received, width: 2560,
                    height: 1440, framesPerSecond: 14.6, bytes: UInt64(420_000 * second),
                    timestamp: 1_700_000_000_000 + second * 1000, lossFraction: 0.021, roundTrip: 0.038,
                    codec: "video/VP9", candidateType: "srflx", transport: "udp"
                ),
            ]
        }
        _ = tracker.update(with: readings(second: 1))
        try await assertSettings(Self.devices, videoStats: tracker.update(with: readings(second: 2)), height: 660)
    }

    func testNoVideoToMeasure() async throws {
        try await assertSettings(Self.devices, videoStats: [], height: 600)
    }

    #if os(iOS)
    /// Only the camera can be chosen: audio follows the system's route. On
    /// the narrowest iPhone, where nothing may be cut off.
    func testOnlyTheCameraOnIOS() async throws {
        try await assertSettings(
            CallDevices(cameras: CallDeviceList(devices: [
                CallDevice(id: "front", name: "Front Camera"), CallDevice(id: "back", name: "Back Camera"),
            ])),
            kinds: [.camera], width: 375, height: 420
        )
    }
    #endif
}
