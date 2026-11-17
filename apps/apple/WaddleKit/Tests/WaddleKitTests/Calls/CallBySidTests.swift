import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

/// Answer and End that arrive from outside the dock name the call they
/// were shown for; by the time they run another may hold the slot.
@MainActor @Suite("Calls named by session") struct CallBySidTests {
    @Test func answeringTheRingingCallSendsItsAnswer() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        #expect(await f.store.answer(sid: "c1"))
        #expect(f.port.callSends.contains(.proceed(callPeer, "c1")))
        // Asked again while it connects, it is answered already.
        #expect(await f.store.answer(sid: "c1"))
        #expect(f.port.callSends.filter { $0 == .proceed(callPeer, "c1") }.count == 1)
        f.store.clear()
    }

    @Test func answeringACallThatIsNotTheOneRingingDoesNothing() async {
        let f = CallFixture()
        #expect(await !f.store.answer(sid: "c1"))
        f.store.handle(f.event(.propose(callAudio)))
        #expect(await !f.store.answer(sid: "other"))
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        f.store.clear()
    }

    @Test func anAnswerThatCannotBeSentIsNotAnAnswer() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        f.port.callResults["proceed"] = false
        #expect(await !f.store.answer(sid: "c1"))
        f.store.clear()
    }

    @Test func endingARingingCallDeclinesIt() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await f.store.end(sid: "c1")
        #expect(f.store.state == .idle)
        #expect(f.port.callSends.contains(.reject(callPeer, "c1", false)))
        f.store.clear()
    }

    @Test func endingAPlacedCallHangsUp() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        await f.store.end(sid: "c1")
        #expect(f.store.state == .idle)
        #expect(f.port.callSends.contains(.retract(callPeer.bare, "c1")))
        f.store.clear()
    }

    @Test func endingACallThatIsNotInTheSlotDoesNothing() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        await f.store.end(sid: "other")
        #expect(f.store.state.isLive)
        f.store.clear()
    }
}
