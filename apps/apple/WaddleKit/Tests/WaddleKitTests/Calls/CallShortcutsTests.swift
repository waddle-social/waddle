import Testing

@testable import WaddleKit

struct CallShortcutsTests {
    @Test func theKeysAreTheWebs() {
        #expect(CallShortcut.allCases.map(\.key) == ["m", "v", "s", "r", "f"])
    }

    @Test func aRoomsCallOnAMacHasEveryKey() {
        let keys = CallShortcut.available(hasMedia: true, canShareScreen: true, hasHands: true, size: .split)
        #expect(keys == CallShortcut.allCases)
    }

    @Test func aCallStillConnectingSwitchesNoDevice() {
        let keys = CallShortcut.available(hasMedia: false, canShareScreen: true, hasHands: true, size: .split)
        #expect(keys == [.hand, .stepUp])
    }

    @Test func aDeviceThatCannotShareLeavesSAlone() {
        let keys = CallShortcut.available(hasMedia: true, canShareScreen: false, hasHands: true, size: .split)
        #expect(!keys.contains(.screenShare))
    }

    @Test func aCallOfTwoHasNoHandToRaise() {
        let keys = CallShortcut.available(hasMedia: true, canShareScreen: true, hasHands: false, size: .split)
        #expect(!keys.contains(.hand))
    }

    @Test func fullScreenIsAsBigAsItGets() {
        #expect(CallShortcut.available(hasMedia: true, canShareScreen: true, hasHands: true, size: .expanded).contains(.stepUp))
        #expect(!CallShortcut.available(hasMedia: true, canShareScreen: true, hasHands: true, size: .immersive).contains(.stepUp))
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
