import Testing

@testable import WaddleKit

struct CallShortcutsTests {
    @Test func theKeysAreTheWebs() {
        #expect(CallShortcut.allCases.map(\.key) == ["m", "v", "s", "r", "f"])
    }

    @Test func eachShortcutAsksForItsStageAction() {
        let actions = CallShortcut.allCases.map { $0.action(size: .split) }
        #expect(actions == [.toggleMicrophone, .toggleCamera, .toggleScreenShare, .toggleHand, .setSize(.expanded)])
    }

    @Test func stepUpUsesTheCurrentSize() {
        #expect(CallShortcut.stepUp.action(size: .expanded) == .setSize(.immersive))
    }

    @Test func aRoomsCallOnAMacHasEveryKey() {
        let controls = CallControls(devices: .init(isSharingScreen: false), isHandRaised: false)
        let keys = CallShortcut.available(for: controls)
        #expect(keys == CallShortcut.allCases)
    }

    @Test func aCallStillConnectingSwitchesNoDevice() {
        let controls = CallControls(devices: .init(hasMedia: false, isSharingScreen: false), isHandRaised: false)
        let keys = CallShortcut.available(for: controls)
        #expect(keys == [.hand, .stepUp])
    }

    @Test func aDeviceThatCannotShareLeavesSAlone() {
        let controls = CallControls(isHandRaised: false)
        let keys = CallShortcut.available(for: controls)
        #expect(!keys.contains(.screenShare))
    }

    @Test func aCallOfTwoHasNoHandToRaise() {
        let controls = CallControls(devices: .init(isSharingScreen: false))
        let keys = CallShortcut.available(for: controls)
        #expect(!keys.contains(.hand))
    }

    @Test func fullScreenIsAsBigAsItGets() {
        var controls = CallControls(devices: .init(isSharingScreen: false), isHandRaised: false, size: .expanded)
        #expect(CallShortcut.available(for: controls).contains(.stepUp))
        controls.size = .immersive
        #expect(!CallShortcut.available(for: controls).contains(.stepUp))
    }
}

struct CallPushToTalkTests {
    private typealias Press = CallPushToTalk.Press

    @Test func holdingSpaceOpensTheMicrophoneAndLettingGoClosesIt() {
        var talk = CallPushToTalk()
        let outcome1 = talk.spaceWentDown(Press())
        #expect(outcome1 == .openMicrophone)
        #expect(talk.isHolding)
        let outcome2 = talk.spaceCameUp()
        #expect(outcome2 == .closeMicrophone)
        #expect(!talk.isHolding)
    }

    @Test func theKeyRepeatingWhileHeldDoesNothingMore() {
        var talk = CallPushToTalk()
        _ = talk.spaceWentDown(Press())
        let outcome3 = talk.spaceWentDown(Press(isRepeat: true, isMicrophoneOn: true))
        #expect(outcome3 == .swallow)
        #expect(talk.isHolding)
    }

    @Test func aMicrophoneAlreadyOpenIsNotClosedByLiftingSpace() {
        var talk = CallPushToTalk()
        let outcome4 = talk.spaceWentDown(Press(isMicrophoneOn: true))
        #expect(outcome4 == .passOn)
        let outcome5 = talk.spaceCameUp()
        #expect(outcome5 == .passOn)
    }

    @Test func spaceBelongsToWhateverHasTheKeyboard() {
        var talk = CallPushToTalk()
        let outcome6 = talk.spaceWentDown(Press(keyboardIsElsewhere: true))
        #expect(outcome6 == .passOn)
        let outcome7 = talk.spaceWentDown(Press(hasModifiers: true))
        #expect(outcome7 == .passOn)
        let outcome8 = talk.spaceWentDown(Press(isRepeat: true))
        #expect(outcome8 == .passOn)
        let outcome9 = talk.spaceWentDown(Press(hasMedia: false))
        #expect(outcome9 == .passOn)
        #expect(!talk.isHolding)
    }

    @Test func aMicrophoneWeOpenedIsClosedEvenIfTheKeyboardHasMovedOn() {
        var talk = CallPushToTalk()
        _ = talk.spaceWentDown(Press())
        // Focus went into a field mid-hold: the repeat is still ours, and so is the release.
        let outcome10 = talk.spaceWentDown(Press(isRepeat: true, keyboardIsElsewhere: true))
        #expect(outcome10 == .swallow)
        let outcome11 = talk.spaceCameUp()
        #expect(outcome11 == .closeMicrophone)
    }

    @Test func losingTheWindowWithTheKeyDownClosesTheMicrophoneOnce() {
        var talk = CallPushToTalk()
        _ = talk.spaceWentDown(Press())
        let outcome12 = talk.release()
        #expect(outcome12)
        let outcome13 = talk.release()
        #expect(!outcome13)
        let outcome14 = talk.spaceCameUp()
        #expect(outcome14 == .passOn)
    }
}

struct CallShortcutRepeatsTests {
    @Test func aKeyHeldDownIsOnePress() {
        var repeats = CallShortcutRepeats()
        let first = repeats.isFreshPress(of: .microphone, at: 0)
        #expect(first)
        // The keyboard starts repeating after half a second, then every few hundredths, for as long as it is held.
        var time = 0.5
        while time < 5 {
            let fresh = repeats.isFreshPress(of: .microphone, at: time)
            #expect(!fresh)
            time += 0.08
        }
    }

    @Test func pressingAgainLaterIsANewPress() {
        var repeats = CallShortcutRepeats()
        _ = repeats.isFreshPress(of: .microphone, at: 0)
        let again = repeats.isFreshPress(of: .microphone, at: 1)
        #expect(again)
    }

    @Test func anotherKeyIsItsOwnPress() {
        var repeats = CallShortcutRepeats()
        _ = repeats.isFreshPress(of: .microphone, at: 0)
        let camera = repeats.isFreshPress(of: .camera, at: 0.1)
        #expect(camera)
    }
}
