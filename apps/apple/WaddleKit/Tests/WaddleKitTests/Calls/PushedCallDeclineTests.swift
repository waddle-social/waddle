import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Declining pushed calls") struct PushedCallDeclineTests {
    @Test func declineBeforeTheProposalSendsNothing() async {
        let f = CallFixture()

        #expect(f.store.declinePushedCall(sid: "c1"))
        await drain(f.store)

        #expect(f.store.state == .idle)
        #expect(f.store.isFinished(sid: "c1"))
        #expect(f.port.callSends.isEmpty)
        #expect(f.store.lastError == nil)
        f.store.clear()
    }

    @Test func aLaterProposalGetsOneRejectToItsFullSenderAndNeverRings() async {
        let f = CallFixture()
        let proposer = JID(parsing: "bob@waddle.test/other-device")!
        #expect(f.store.declinePushedCall(sid: "c1"))

        f.store.handle(f.event(.propose(callAudio), from: proposer))
        f.store.handle(f.event(.propose(callAudio), from: proposer))
        await drain(f.store)
        f.store.handle(f.event(.propose(callAudio), from: proposer))
        await drain(f.store)

        #expect(f.store.state == .idle)
        #expect(f.store.isFinished(sid: "c1"))
        #expect(f.port.callSends == [.reject(proposer, "c1", false)])
        f.store.clear()
    }

    @Test(arguments: [JID(parsing: "bob@waddle.test")!, callOwn])
    func aBareOrSelfOriginatedProposalConsumesTheDeclineWithoutSending(from: JID) async {
        let f = CallFixture()
        #expect(f.store.declinePushedCall(sid: "c1"))

        f.store.handle(f.event(.propose(callAudio), from: from))
        await drain(f.store)
        f.store.handle(f.event(.propose(callAudio)))
        await drain(f.store)

        #expect(f.store.state == .idle)
        #expect(f.port.callSends.isEmpty)
        f.store.clear()
    }

    @Test func declineLeavesAnotherLiveCallAlone() async {
        let f = CallFixture(sid: { "c-other" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callVideo))
        let before = f.store.state

        #expect(f.store.declinePushedCall(sid: "c1"))
        #expect(f.port.callSends == [.propose(callPeer.bare, "c-other", callVideo)])
        f.store.handle(f.event(.propose(callAudio)))
        await drain(f.store)

        #expect(f.store.state == before)
        #expect(f.store.isFinished(sid: "c1"))
        #expect(f.port.callSends == [.propose(callPeer.bare, "c-other", callVideo), .reject(callPeer, "c1", false)])
        f.store.clear()
    }

    @Test(arguments: [false, true]) func anAdoptedCallCannotBeRejectedBehindTheSlot(accepting: Bool) async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await drain(f.store)
        if accepting { #expect(await f.store.acceptIncoming()) }
        let before = f.store.state

        #expect(!f.store.declinePushedCall(sid: "c1"))
        await drain(f.store)

        #expect(f.store.state == before)
        #expect(!f.store.isFinished(sid: "c1"))
        #expect(!f.port.callSends.contains(.reject(callPeer, "c1", false)))
        f.store.clear()
    }

    @Test func failedRejectRemainsFinishedAndIsNotRetried() async {
        let f = CallFixture()
        f.port.callResults["reject"] = false
        #expect(f.store.declinePushedCall(sid: "c1"))
        f.store.handle(f.event(.propose(callAudio)))
        await drain(f.store)
        f.store.handle(f.event(.propose(callAudio)))
        await drain(f.store)

        #expect(f.store.state == .idle)
        #expect(f.store.isFinished(sid: "c1"))
        #expect(f.port.callSends == [.reject(callPeer, "c1", false)])
        #expect(f.store.lastError == "call reject failed")
        f.store.clear()
    }

    @Test func unconsumedDeclinesSurviveClear() async {
        let f = CallFixture()
        #expect(f.store.declinePushedCall(sid: "c1"))
        f.store.clear()

        f.store.handle(f.event(.propose(callAudio)))
        await drain(f.store)

        #expect(f.store.state == .idle)
        #expect(f.store.isFinished(sid: "c1"))
        #expect(f.port.callSends == [.reject(callPeer, "c1", false)])
        f.store.clear()
    }

    @Test func declineHistoryRetainsOnly32DistinctSids() async {
        let f = CallFixture()
        for number in 0..<32 { #expect(f.store.declinePushedCall(sid: "c\(number)")) }
        #expect(f.store.declinePushedCall(sid: "c0"))
        #expect(f.store.declinePushedCall(sid: "c32"))
        // Restore only finished history: the evicted decline must stay evicted.
        f.store.markFinished(sid: "c0")

        f.store.handle(f.event(.propose(callAudio), sid: "c0"))
        f.store.handle(f.event(.propose(callAudio), sid: "c32"))
        await drain(f.store)

        #expect(f.store.state == .idle)
        #expect(f.port.callSends == [.reject(callPeer, "c32", false)])
        f.store.clear()
    }

    @Test func clearDiscardsAQueuedRejectBeforeTheNextSession() async {
        let f = CallFixture(sid: { "c-new" })
        var release: CheckedContinuation<Void, Never>?
        var released = false
        f.store.enqueue {
            await withCheckedContinuation { release = $0 }
            released = true
        }
        await eventually { release != nil }
        #expect(release != nil)
        #expect(f.store.declinePushedCall(sid: "c1"))
        f.store.handle(f.event(.propose(callAudio)))

        f.store.clear()
        #expect(await f.store.startCall(to: callPeer.bare, media: callVideo))
        release?.resume()
        await eventually { released }
        #expect(released)
        f.store.handle(f.event(.propose(callAudio)))
        await drain(f.store)

        #expect(f.port.callSends == [.propose(callPeer.bare, "c-new", callVideo)])
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c-new", media: callVideo, initiator: callOwn)))
        #expect(f.store.lastError == nil)
        f.store.clear()
    }

    @Test(arguments: [true, false]) func clearFencesASuspendedReject(result: Bool) async {
        let f = CallFixture(sid: { "c-new" })
        f.port.heldCallVerbs = ["reject"]
        f.port.callResults["reject"] = result
        #expect(f.store.declinePushedCall(sid: "c1"))
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends == [.reject(callPeer, "c1", false)] }
        #expect(f.port.callSends == [.reject(callPeer, "c1", false)])

        f.store.clear()
        f.port.heldCallVerbs = []
        #expect(await f.store.startCall(to: callPeer.bare, media: callVideo))
        f.port.releaseCallSends()
        await eventually { f.port.completedCallSends.contains(.reject(callPeer, "c1", false)) }
        #expect(f.port.completedCallSends.contains(.reject(callPeer, "c1", false)))
        await drain(f.store)

        #expect(f.port.callSends == [.reject(callPeer, "c1", false), .propose(callPeer.bare, "c-new", callVideo)])
        #expect(f.store.lastError == nil)
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c-new", media: callVideo, initiator: callOwn)))
        #expect(f.store.isFinished(sid: "c1"))
        f.store.clear()
    }

    private func drain(_ store: CallStore) async {
        var drained = false
        store.enqueue { drained = true }
        await eventually { drained }
        #expect(drained)
    }
}
