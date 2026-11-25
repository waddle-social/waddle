import Testing

@testable import WaddleKit

@Suite("Call devices") struct CallDevicesTests {
    private static let builtIn = CallDevice(id: "builtin", name: "MacBook Pro Microphone")
    private static let headset = CallDevice(id: "usb-1", name: "Headset")

    @Test func withNoChoiceTheSystemsDeviceIsUsed() {
        let list = CallDeviceList(devices: [Self.builtIn, Self.headset])
        #expect(list.chosen == nil)
        #expect(!list.isChoiceMissing)
    }

    @Test func aChosenDeviceIsUsedWhileItIsConnected() {
        let list = CallDeviceList(devices: [Self.builtIn, Self.headset], preferred: "usb-1")
        #expect(list.chosen == Self.headset)
        #expect(!list.isChoiceMissing)
    }

    @Test func aChosenDeviceThatIsGoneGivesWayToTheSystemAndIsRemembered() {
        let unplugged = CallDeviceList(devices: [Self.builtIn], preferred: "usb-1")
        #expect(unplugged.chosen == nil)
        #expect(unplugged.isChoiceMissing)
        // Plugged back in, it is the choice again without being asked for.
        let back = CallDeviceList(devices: [Self.builtIn, Self.headset], preferred: unplugged.preferred)
        #expect(back.chosen == Self.headset)
    }

    @Test func preferencesAreKeptPerKindAndSurviveStorage() {
        var preferences = CallDevicePreferences()
        preferences[.microphone] = "usb-1"
        preferences[.camera] = "cam-9"
        let restored = CallDevicePreferences(stored: preferences.stored)
        #expect(restored == preferences)
        #expect(restored[.microphone] == "usb-1")
        #expect(restored[.speaker] == nil)
    }

    @Test func clearingAChoiceGoesBackToTheSystem() {
        var preferences = CallDevicePreferences()
        preferences[.speaker] = "hdmi"
        preferences[.speaker] = nil
        #expect(preferences == CallDevicePreferences())
        preferences[.speaker] = ""
        #expect(preferences[.speaker] == nil)
    }

    @Test func whatIsNotAPreferenceInStorageIsIgnored() {
        let restored = CallDevicePreferences(stored: ["microphone": 7, "camera": "cam-9", "unknown": "x"])
        #expect(restored[.microphone] == nil)
        #expect(restored[.camera] == "cam-9")
        #expect(CallDevicePreferences(stored: nil) == CallDevicePreferences())
    }

    @Test func devicesAreReadByKind() {
        let devices = CallDevices(microphones: CallDeviceList(devices: [Self.builtIn]))
        #expect(devices[.microphone].devices == [Self.builtIn])
        #expect(devices[.camera].devices.isEmpty)
    }

    // MARK: - Audio processing

    @Test func allProcessingIsOnUntilTurnedOff() {
        #expect(CallAudioProcessing() == CallAudioProcessing(noiseSuppression: true, echoCancellation: true, autoGainControl: true))
        #expect(CallAudioProcessing(stored: nil) == CallAudioProcessing())
    }

    @Test func aProcessingChoiceSurvivesStorage() {
        let chosen = CallAudioProcessing(noiseSuppression: false, echoCancellation: true, autoGainControl: false)
        #expect(CallAudioProcessing(stored: chosen.stored) == chosen)
    }

    @Test func halfAStoredChoiceIsTheDefaults() {
        #expect(CallAudioProcessing(stored: ["noiseSuppression": false]) == CallAudioProcessing())
        #expect(CallAudioProcessing(stored: ["noiseSuppression": false, "echoCancellation": "no", "autoGainControl": true]) == CallAudioProcessing())
        #expect(CallAudioProcessing(stored: "nonsense") == CallAudioProcessing())
    }

    // MARK: - Camera background

    @Test func theBackgroundIsTheRoomUntilChosen() {
        #expect(CallBackground(stored: nil) == .off)
    }

    @Test func aChosenBackgroundSurvivesStorage() {
        #expect(CallBackground(stored: CallBackground.blur.rawValue) == .blur)
        #expect(CallBackground(stored: CallBackground.off.rawValue) == .off)
    }

    @Test func aBackgroundThisAppDoesNotHaveIsTheRoom() {
        #expect(CallBackground(stored: "image") == .off)
        #expect(CallBackground(stored: "") == .off)
    }
}
