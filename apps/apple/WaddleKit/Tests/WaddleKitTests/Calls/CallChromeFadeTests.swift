import Testing

@testable import WaddleKit

struct CallChromeFadeTests {
    @Test func theHeaderAndControlsShowToBeginWith() {
        #expect(CallChromeFade().isShown)
    }

    @Test func theyHideOnceThePointerHasRestedTwoSeconds() {
        var fade = CallChromeFade()
        fade.stirred(at: 10)
        #expect(fade.check(at: 12, isHeld: false) == nil)
        #expect(!fade.isShown)
    }

    @Test func aMoveDuringTheWaitStartsItAgain() {
        var fade = CallChromeFade()
        fade.stirred(at: 10)
        fade.stirred(at: 11.5)
        #expect(fade.check(at: 12, isHeld: false) == 1.5)
        #expect(fade.isShown)
    }

    @Test func aMoveBringsThemBack() {
        var fade = CallChromeFade()
        fade.stirred(at: 10)
        _ = fade.check(at: 12, isHeld: false)
        fade.stirred(at: 20)
        #expect(fade.isShown)
        #expect(fade.check(at: 20, isHeld: false) == 2)
    }

    @Test func somethingOpenKeepsThemAndIsAskedAboutAgainAfterAFullRest() {
        var fade = CallChromeFade()
        fade.stirred(at: 10)
        #expect(fade.check(at: 12, isHeld: true) == 2)
        #expect(fade.isShown)
        #expect(fade.check(at: 14, isHeld: false) == nil)
        #expect(!fade.isShown)
    }
}
