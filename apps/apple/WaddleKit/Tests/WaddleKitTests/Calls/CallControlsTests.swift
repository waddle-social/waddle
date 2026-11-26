import Testing

@testable import WaddleKit

struct CallControlsTests {
    @Test func defaultsMatchTheControlBar() {
        let controls = CallControls()
        #expect(!controls.devices.isMicrophoneOn)
        #expect(!controls.devices.isCameraOn)
        #expect(controls.devices.hasMedia)
        #expect(controls.devices.isSharingScreen == nil)
        #expect(controls.isHandRaised == nil)
        #expect(controls.viewMode == .gallery)
        #expect(!controls.isSelfViewHidden)
        #expect(controls.size == .split)
        #expect(controls.escapeStepsDown)
        #expect(!controls.showsParticipants)
        #expect(controls.chatUnread == nil)
        #expect(!controls.showsChat)
        #expect(!controls.canReact)
    }

    @Test func defaultShortcutsSwitchDevicesAndStepUp() {
        #expect(CallShortcut.available(for: CallControls()) == [.microphone, .camera, .stepUp])
    }

    @Test func fullScreenWithAHandUpAndScreenSharingCanStillToggleBoth() {
        let controls = CallControls(devices: .init(isSharingScreen: true), isHandRaised: true, size: .immersive)
        #expect(CallShortcut.available(for: controls) == [.microphone, .camera, .screenShare, .hand])
    }

    @Test func reactionsDoNotEnableDeviceShortcutsWithoutMedia() {
        let controls = CallControls(devices: .init(hasMedia: false, isSharingScreen: true), isHandRaised: true, canReact: true)
        #expect(CallShortcut.available(for: controls) == [.hand, .stepUp])
    }
}
