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
        audioProcessing: CallAudioProcessing = CallAudioProcessing(), microphoneMode: CallMicrophoneMode? = .standard,
        background: CallBackground = .off,
        videoStats: [CallVideoStat]? = nil,
        height: CGFloat = 620, testName: String = #function, line: UInt = #line
    ) async throws {
        try await assertScreenSnapshot(
            of: CallSettingsView(
                devices: devices, kinds: kinds, audioProcessing: audioProcessing, microphoneMode: microphoneMode,
                background: background,
                videoStats: videoStats, choose: { _, _ in /* drawn only */ }, done: { /* drawn only */ }
            ),
            size: CGSize(width: width, height: height), testName: testName, line: line
        )
    }

    func testAChosenMicrophoneAndTheSystemsSpeakerAndCamera() async throws {
        try await assertSettings(
            CallDevices(
                microphones: CallDeviceList(devices: [Self.builtIn, Self.headset], preferred: Self.headset.id),
                speakers: CallDeviceList(devices: [Self.speakers]),
                cameras: CallDeviceList(devices: [Self.faceTime])
            ),
            audioProcessing: CallAudioProcessing(noiseSuppression: false), microphoneMode: .voiceIsolation
        )
    }

    /// The chosen headset is unplugged and no camera is found.
    func testAChosenDeviceThatIsGoneAndNoCamera() async throws {
        try await assertSettings(
            CallDevices(
                microphones: CallDeviceList(devices: [Self.builtIn], preferred: Self.headset.id),
                speakers: CallDeviceList(devices: [Self.speakers])
            ),
            audioProcessing: CallAudioProcessing(noiseSuppression: false, autoGainControl: false),
            microphoneMode: nil, height: 600
        )
    }

    private static let devices = CallDevices(
        microphones: CallDeviceList(devices: [builtIn]), speakers: CallDeviceList(devices: [speakers]),
        cameras: CallDeviceList(devices: [faceTime])
    )

    /// Our camera going out and someone's screen coming in, as the tracker writes them.
    func testVideoStatistics() async throws {
        var tracker = CallVideoStatsTracker()
        func reading(
            _ key: String, _ label: String, _ source: CallVideoSource, _ direction: CallVideoSample.Direction,
            size: (Int, Int), fps: Double, bytesPerSecond: Double, loss: Double, at second: Double
        ) -> CallVideoSample {
            var sample = CallVideoSample(
                key: key, label: label, source: source, direction: direction, bytes: UInt64(bytesPerSecond * second),
                timestamp: 1_700_000_000_000 + second * 1000
            )
            (sample.width, sample.height, sample.framesPerSecond) = (size.0, size.1, fps)
            (sample.lossFraction, sample.roundTrip) = (loss, 0.038)
            (sample.codec, sample.candidateType, sample.transport) = ("video/VP9", "srflx", "udp")
            return sample
        }
        func readings(second: Double) -> [CallVideoSample] {
            [
                reading("me:camera:sent", "You", .camera, .sent, size: (1280, 720), fps: 30, bytesPerSecond: 150_000, loss: 0.004, at: second),
                reading("linus:screen:received", "linus", .screen, .received, size: (2560, 1440), fps: 14.6, bytesPerSecond: 420_000, loss: 0.021, at: second),
            ]
        }
        _ = tracker.update(with: readings(second: 1))
        try await assertSettings(Self.devices, videoStats: tracker.update(with: readings(second: 2)), height: 760)
    }

    func testABlurredBackground() async throws {
        try await assertSettings(Self.devices, background: .blur, height: 620)
    }

    func testNoVideoToMeasure() async throws {
        try await assertSettings(Self.devices, videoStats: [], height: 700)
    }

    #if os(iOS)
    /// Only the camera can be chosen: audio follows the system's route. On
    /// the narrowest iPhone, where nothing may be cut off.
    func testOnlyTheCameraOnIOS() async throws {
        try await assertSettings(
            CallDevices(cameras: CallDeviceList(devices: [
                CallDevice(id: "front", name: "Front Camera"), CallDevice(id: "back", name: "Back Camera"),
            ])),
            kinds: [.camera], width: 375, microphoneMode: .wideSpectrum, height: 520
        )
    }
    #endif
}
