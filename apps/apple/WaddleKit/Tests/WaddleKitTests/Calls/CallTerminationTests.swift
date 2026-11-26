import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Call termination") struct CallTerminationTests {
    @Test func quittingEndsAnActiveCall() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.proceed))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        await eventually { f.port.completedCallSends.count == 2 }
        f.port.completedCallSends = []

        var sendsAtCompletion: [CallSend]?
        #expect(f.store.hangUpForTermination { sendsAtCompletion = f.port.completedCallSends } != nil)
        await eventually(timeout: 1) { sendsAtCompletion != nil }

        #expect(f.store.state == .idle)
        #expect(sendsAtCompletion == [.terminate(callPeer, "c1", .success), .finish(callPeer, "c1", nil)])
    }

    @Test func quittingLeavesTheRoomCall() async {
        let f = GroupFixture()
        await f.activate()
        f.port.completedCallSends = []

        var sendsAtCompletion: [CallSend]?
        #expect(f.store.hangUpForTermination { sendsAtCompletion = f.port.completedCallSends } != nil)
        await eventually { sendsAtCompletion != nil }

        #expect(f.store.state == .idle)
        #expect(sendsAtCompletion == [
            .groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1"),
        ])
    }

    @Test func quittingWithoutACallDoesNotWait() {
        let f = CallFixture()

        #expect(f.store.hangUpForTermination { Issue.record("No hang-up was started") } == nil)
        #expect(f.store.state == .idle)
        #expect(f.port.callSends.isEmpty)
    }

    @Test func quittingLeavesAnUnansweredIncomingCallAlone() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.completedCallSends.count == 1 }

        #expect(f.store.hangUpForTermination { Issue.record("The call was never answered here") } == nil)
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        #expect(f.port.callSends == [.ringing(callPeer, "c1")])
        f.store.clear()
    }

    @Test func quittingRetractsAnOutgoingRing() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))

        var sendsAtCompletion: [CallSend]?
        #expect(f.store.hangUpForTermination { sendsAtCompletion = f.port.completedCallSends } != nil)
        await eventually { sendsAtCompletion != nil }

        #expect(f.store.state == .idle)
        #expect(sendsAtCompletion == [.propose(callPeer.bare, "c1", callAudio), .retract(callPeer.bare, "c1")])
    }

    @Test func quittingFinishesACallWeAnsweredThatIsStillConnecting() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.completedCallSends.count == 1 }
        #expect(await f.store.acceptIncoming())

        var sendsAtCompletion: [CallSend]?
        #expect(f.store.hangUpForTermination { sendsAtCompletion = f.port.completedCallSends } != nil)
        await eventually { sendsAtCompletion != nil }

        #expect(f.store.state == .idle)
        #expect(sendsAtCompletion == [.ringing(callPeer, "c1"), .proceed(callPeer, "c1"), .finish(callPeer, "c1", .cancel)])
    }

    @Test func quittingDoesNotHangUpAReplacementRing() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))

        var completed = false
        #expect(f.store.hangUpForTermination { completed = true } != nil)
        f.store.clear()
        f.store.handle(f.event(.propose(callAudio), sid: "c2"))
        await eventually { completed && f.port.completedCallSends.count >= 2 }

        #expect(completed)
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c2", media: callAudio)))
        #expect(f.port.callSends == [.propose(callPeer.bare, "c1", callAudio), .ringing(callPeer, "c2")])
        f.store.clear()
    }

    @Test func quittingStopsWaitingEvenWhenTheSendDoesNotComplete() async throws {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.port.heldCallVerbs = ["retract"]

        var completions: [ContinuousClock.Instant] = []
        let deadline = try #require(f.store.hangUpForTermination(timeout: 0.5) {
            completions.append(.now)
        })
        await eventually { !completions.isEmpty }

        #expect(completions.count == 1)
        if let completion = completions.first { #expect(completion >= deadline) }
        #expect(f.port.callSends.last == .retract(callPeer.bare, "c1"))
        #expect(f.port.completedCallSends == [.propose(callPeer.bare, "c1", callAudio)])

        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        await eventually { f.port.completedCallSends.last == .retract(callPeer.bare, "c1") }
        #expect(completions.count == 1)
    }
}
