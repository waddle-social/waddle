import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Declining pushed calls") struct PushedCallDeclineTests {
    @Test func declineWithoutAFullSenderStaysLocal() async {
        let f = CallFixture()
        let bareSender = JID(parsing: callPeer.bare.description)!

        #expect(await f.store.declinePushedCall(sid: "missing-from", from: nil))
        #expect(await f.store.declinePushedCall(sid: "bare-from", from: bareSender))

        #expect(f.store.isFinished(sid: "missing-from"))
        #expect(f.store.isFinished(sid: "bare-from"))
        #expect(f.port.callSends.isEmpty)
        #expect(f.store.lastError == nil)
        f.store.clear()
    }

    @Test func declineWithoutASlotSendsOnlyRejectAndSuppressesALateProposal() async {
        let f = CallFixture()
        f.port.heldCallVerbs = ["reject"]
        let decline = Task { await f.store.declinePushedCall(sid: "c1", from: callPeer) }
        await eventually { f.port.callSends == [.reject(callPeer, "c1", false)] }

        #expect(f.store.state == .idle)
        #expect(f.store.isFinished(sid: "c1"))
        f.store.handle(f.event(.propose(callAudio)))
        #expect(f.store.state == .idle)
        #expect(f.port.callSends == [.reject(callPeer, "c1", false)])

        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await decline.value)
        f.store.clear()
    }

    @Test func declineLeavesAnotherLiveCallAlone() async {
        let f = CallFixture(sid: { "c-other" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callVideo))
        let before = f.store.state

        #expect(await f.store.declinePushedCall(sid: "c1", from: callPeer))

        #expect(f.store.state == before)
        #expect(f.store.isFinished(sid: "c1"))
        #expect(f.port.callSends == [.propose(callPeer.bare, "c-other", callVideo), .reject(callPeer, "c1", false)])
        f.store.clear()
    }

    @Test func anAdoptedCallCannotBeRejectedBehindTheSlot() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        #expect(await f.store.acceptIncoming())
        let before = f.store.state

        #expect(await !f.store.declinePushedCall(sid: "c1", from: callPeer))

        #expect(f.store.state == before)
        #expect(!f.store.isFinished(sid: "c1"))
        #expect(!f.port.callSends.contains(.reject(callPeer, "c1", false)))
        f.store.clear()
    }

    @Test func failedRejectRemainsFinishedAndReportsTheError() async {
        let f = CallFixture()
        f.port.callResults["reject"] = false

        #expect(await !f.store.declinePushedCall(sid: "c1", from: callPeer))

        #expect(f.store.state == .idle)
        #expect(f.store.isFinished(sid: "c1"))
        #expect(f.store.lastError == "call reject failed")
        f.store.clear()
    }

    @Test(arguments: [true, false]) func sessionClearFencesASuspendedReject(result: Bool) async {
        let f = CallFixture(sid: { "c-new" })
        f.port.heldCallVerbs = ["reject"]
        f.port.callResults["reject"] = result
        let decline = Task { await f.store.declinePushedCall(sid: "c1", from: callPeer) }
        await eventually { f.port.callSends == [.reject(callPeer, "c1", false)] }

        f.store.clear()
        f.port.heldCallVerbs = []
        #expect(await f.store.startCall(to: callPeer.bare, media: callVideo))
        f.port.releaseCallSends()

        #expect(await !decline.value)
        #expect(f.store.lastError == nil)
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c-new", media: callVideo, initiator: callOwn)))
        #expect(f.store.isFinished(sid: "c1"))
        f.store.clear()
    }
}
