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
        height: CGFloat = 340, testName: String = #function, line: UInt = #line
    ) async throws {
        try await assertScreenSnapshot(
            of: CallSettingsView(devices: devices, kinds: kinds, choose: { _, _ in /* drawn only */ }, done: { /* drawn only */ }),
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
            height: 380
        )
    }

    #if os(iOS)
    /// Only the camera can be chosen: audio follows the system's route. On
    /// the narrowest iPhone, where nothing may be cut off.
    func testOnlyTheCameraOnIOS() async throws {
        try await assertSettings(
            CallDevices(cameras: CallDeviceList(devices: [
                CallDevice(id: "front", name: "Front Camera"), CallDevice(id: "back", name: "Back Camera"),
            ])),
            kinds: [.camera], width: 375, height: 260
        )
    }
    #endif
}
