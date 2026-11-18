import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Finished calls") struct CallFinishedTests {
    @Test func declinedCallDoesNotRingAgainFromALateProposal() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends == [.ringing(callPeer.bare, "c1")] }
        #expect(await f.store.declineIncoming())
        #expect(f.store.isFinished(sid: "c1"))
        f.port.callSends = []

        f.store.handle(f.event(.propose(callAudio)))
        await drain(f.store)

        #expect(f.store.state == .idle)
        #expect(f.port.callSends.isEmpty)
        f.store.clear()
    }

    @Test func externallyFinishedCallNeverTakesTheSlotOrSendsAnEffect() async {
        let f = CallFixture()
        f.store.markFinished(sid: "c1")
        f.store.handle(f.event(.propose(callAudio)))
        await drain(f.store)

        #expect(f.store.state == .idle)
        #expect(f.port.callSends.isEmpty)
        #expect(f.store.isFinished(sid: "c1"))
        f.store.clear()
    }

    @Test func historyRetainsTheLast32DistinctSidsInArrivalOrder() {
        let f = CallFixture()
        for number in 0..<32 { f.store.markFinished(sid: "c\(number)") }
        for number in 0..<32 { #expect(f.store.isFinished(sid: "c\(number)")) }

        // Repeated observations neither consume capacity nor refresh age.
        f.store.markFinished(sid: "c0")
        f.store.markFinished(sid: "c32")
        #expect(!f.store.isFinished(sid: "c0"))
        for number in 1...32 { #expect(f.store.isFinished(sid: "c\(number)")) }

        for number in 33...64 { f.store.markFinished(sid: "c\(number)") }
        for number in 0...32 { #expect(!f.store.isFinished(sid: "c\(number)")) }
        for number in 33...64 { #expect(f.store.isFinished(sid: "c\(number)")) }
        f.store.clear()
    }

    @Test func remoteEndAndClearRetainFinishedHistory() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        f.store.handle(f.event(.retract(reason: nil, tieBreak: false)))
        #expect(f.store.state == .ended(sid: "c1", reason: .retracted))
        #expect(f.store.isFinished(sid: "c1"))

        f.store.handle(f.event(.propose(callAudio), sid: "c2"))
        #expect(f.store.state.sid == "c2")
        f.store.clear()
        #expect(f.store.isFinished(sid: "c1"))
        #expect(f.store.isFinished(sid: "c2"))
        f.port.callSends = []
        f.store.handle(f.event(.propose(callAudio), sid: "c2"))
        await drain(f.store)
        #expect(f.store.state == .idle)
        #expect(f.port.callSends.isEmpty)
    }

    @Test func siblingAnswerFinishesTheLocalRing() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        f.store.handle(f.event(.proceed, from: callOwn))
        #expect(f.store.state == .idle)
        #expect(f.store.isFinished(sid: "c1"))
        f.store.handle(f.event(.propose(callAudio)))
        await drain(f.store)
        #expect(f.store.state == .idle)
        #expect(f.port.callSends.isEmpty)
        f.store.clear()
    }

    @Test(arguments: [false, true]) func timeoutFinishesTheSession(outgoing: Bool) async {
        let f = CallFixture(ringTimeout: .milliseconds(30), unansweredTimeout: .milliseconds(30))
        if outgoing {
            #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        } else {
            f.store.handle(f.event(.propose(callAudio)))
        }
        await eventually { f.store.isFinished(sid: "c1") }
        #expect(f.store.state == .ended(sid: "c1", reason: outgoing ? .timeout : .missed))
        f.store.clear()
        #expect(f.store.isFinished(sid: "c1"))
        f.store.handle(f.event(.propose(callAudio)))
        await drain(f.store)
        #expect(f.store.state == .idle)
    }

    @Test func finishedProposalCannotBeInstalledByASuspendedTieBreak() async {
        let f = CallFixture(sid: { "c-zz" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.port.heldCallVerbs = ["retractTieBreak"]
        f.store.handle(f.event(.propose(callAudio), sid: "c-aa"))
        await eventually { f.port.callSends.last == .retractTieBreak(callPeer, "c-zz") }

        f.store.markFinished(sid: "c-aa")
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        await drain(f.store)

        #expect(f.store.state == .ended(sid: "c-zz", reason: .expired))
        #expect(f.store.isFinished(sid: "c-aa"))
        #expect(!f.port.callSends.contains(.ringing(callPeer.bare, "c-aa")))
        f.store.clear()
    }

    @Test(arguments: [false, true])
    func finishedProposalCannotBeInstalledByASuspendedMigration(oldCallEndsRemotely: Bool) async {
        let f = CallFixture(sid: { "c-old" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio), sid: "c-old"))
        f.port.heldCallVerbs = ["migrated"]
        f.store.handle(f.event(.propose(callAudio), sid: "c-new"))
        await eventually { f.port.callSends.last == .migrated(callPeer, "c-old", "c-new") }

        f.store.markFinished(sid: "c-new")
        if oldCallEndsRemotely {
            f.store.handle(f.event(.finish(reason: .success, migratedTo: nil), sid: "c-old"))
        }
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        await drain(f.store)

        #expect(!f.store.state.isLive)
        #expect(f.store.isFinished(sid: "c-new"))
        #expect(!f.port.callSends.contains(.proceed(callPeer, "c-new")))
        #expect(!f.port.callSends.contains(.ringing(callPeer.bare, "c-new")))
        f.store.clear()
    }

    @Test func remoteAbortHistoryIsSeparateAndStillClearedWithTheSession() {
        let f = CallFixture()
        f.store.handle(f.event(.retract(reason: nil, tieBreak: false), sid: "c-unseen"))
        #expect(f.store.recentlyAbortedSids == ["c-unseen"])
        #expect(!f.store.isFinished(sid: "c-unseen"))
        f.store.markFinished(sid: "c-finished")
        #expect(f.store.recentlyAbortedSids == ["c-unseen"])

        f.store.clear()
        #expect(f.store.recentlyAbortedSids.isEmpty)
        #expect(f.store.isFinished(sid: "c-finished"))
    }

    private func drain(_ store: CallStore) async {
        var drained = false
        store.enqueue { drained = true }
        await eventually { drained }
        #expect(drained)
    }
}
