import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite struct CallStoreTieBreakTests {
    @Test func unrelatedCallerIsBusyRejected() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        let carol = JID(parsing: "carol@waddle.test/tab")!
        f.store.handle(f.event(.propose(callAudio), sid: "c2", from: carol))
        await eventually { f.port.callSends.count == 2 }
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1"), .reject(carol, "c2", false)])
        f.store.clear()
    }
    @Test func lowerIncomingSIDRetractsOursAndTakesTheirs() async {
        let f = CallFixture(sid: { "c-zz" }, ringTimeout: .milliseconds(250))
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.propose(callAudio), sid: "c-aa"))
        await eventually { f.store.state.isIncoming }
        let clock = CallFixture(ringTimeout: .milliseconds(500))
        #expect(await clock.store.startCall(to: callPeer.bare, media: callAudio))
        await eventually { clock.port.callSends.count == 2 }
        #expect(
            f.port.callSends == [
                .propose(callPeer.bare, "c-zz", callAudio), .retractTieBreak(callPeer, "c-zz"),
            ])
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c-aa", media: callAudio)))
        f.store.clear()
        clock.store.clear()
    }

    @Test func higherIncomingSIDGetsTieBreakReject() async {
        let f = CallFixture(sid: { "c-aa" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.propose(callAudio), sid: "c-zz"))
        await eventually { f.port.callSends.count == 2 }
        #expect(f.port.callSends == [.propose(callPeer.bare, "c-aa", callAudio), .reject(callPeer, "c-zz", true)])
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c-aa", media: callAudio, initiator: callOwn)))
        f.store.clear()
    }

    @Test func tieBreakRejectEndsOursAsExpired() async {
        let f = CallFixture(sid: { "c-zz" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.reject(reason: .expired, tieBreak: true), sid: "c-zz"))
        #expect(f.store.state == .ended(sid: "c-zz", reason: .expired))
        f.store.clear()
    }

    @Test func activePeerReproposeMigratesThenTerminatesOldSession() async {
        let f = CallFixture(sid: { "c-old" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio), sid: "c-old"))
        f.port.callSends = []
        f.store.handle(f.event(.propose(callVideo), sid: "c-new"))
        await eventually { f.port.callSends.count == 3 }
        #expect(
            f.port.callSends == [
                .migrated(callPeer, "c-old", "c-new"), .proceed(callPeer, "c-new"), .terminate(callPeer, "c-old", .expired),
            ])
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c-new", media: callVideo, isAccepting: true)))
        f.store.clear()
    }

    @Test func acceptingRingTimesOutIfCallerNeverInitiates() async {
        let f = CallFixture(sessionTimeout: .milliseconds(30))
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        #expect(await f.store.acceptIncoming())
        await eventually { f.port.callSends.count == 3 }
        #expect(f.store.state == .ended(sid: "c1", reason: .timeout))
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1"), .proceed(callPeer, "c1"), .finish(callPeer, "c1", .timeout)])
        f.store.clear()
    }

    @Test func sessionInitiateRetiresResponderTimer() async {
        let f = CallFixture(sessionTimeout: .milliseconds(30))
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        #expect(await f.store.acceptIncoming())
        f.store.handle(f.event(.sessionInitiate(join: callJoin, media: callAudio)))
        await eventually { f.port.callSends.count == 3 }
        // Another public timeout proves the old deadline has elapsed.
        let clock = CallFixture(ringTimeout: .milliseconds(60))
        #expect(await clock.store.startCall(to: callPeer.bare, media: callAudio))
        await eventually { clock.port.callSends.count == 2 }
        #expect(f.store.state == .active(ActiveCall(peer: callPeer, sid: "c1", media: callAudio, join: callJoin, initiator: callPeer)))
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1"), .proceed(callPeer, "c1"), .accept(callPeer, callOwn, "c1", callAudio)])
        f.store.clear()
        clock.store.clear()
    }

    @Test func migratedRingTimesOutIfCallerNeverInitiates() async {
        let f = CallFixture(sid: { "c-old" }, sessionTimeout: .milliseconds(30))
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio), sid: "c-old"))
        f.port.callSends = []
        f.store.handle(f.event(.propose(callAudio), sid: "c-new"))
        await eventually { f.port.callSends.count == 4 }
        #expect(f.store.state == .ended(sid: "c-new", reason: .timeout))
        #expect(
            f.port.callSends == [
                .migrated(callPeer, "c-old", "c-new"), .proceed(callPeer, "c-new"), .terminate(callPeer, "c-old", .expired),
                .finish(callPeer, "c-new", .timeout),
            ])
        f.store.clear()
    }

    @Test func expiredRejectWhileWinnerQueuedStillRingsWinner() async {
        let f = CallFixture(sid: { "c-zz" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.propose(callAudio), sid: "c-aa"))
        f.store.handle(f.event(.reject(reason: .expired, tieBreak: true), sid: "c-zz"))
        await eventually { f.port.callSends.count == 2 }
        #expect(f.port.callSends == [.propose(callPeer.bare, "c-zz", callAudio), .ringing(callPeer.bare, "c-aa")])
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c-aa", media: callAudio)))
        f.store.clear()
    }

    @Test func queuedWinnerAlreadyRetractedNeverRings() async {
        let f = CallFixture(sid: { "c-zz" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.propose(callAudio), sid: "c-aa"))
        f.store.handle(f.event(.retract(reason: nil, tieBreak: false), sid: "c-aa"))
        f.store.handle(f.event(.reject(reason: .expired, tieBreak: true), sid: "c-zz"))
        // A following live invitation is a public barrier for queued effects.
        f.store.handle(f.event(.propose(callAudio), sid: "c-next"))
        await eventually { f.port.callSends.contains(.ringing(callPeer.bare, "c-next")) }
        #expect(f.port.callSends == [.propose(callPeer.bare, "c-zz", callAudio), .ringing(callPeer.bare, "c-next")])
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c-next", media: callAudio)))
        f.store.clear()
    }

    @Test func expiredRejectDuringRetractStillRingsWinner() async {
        let f = CallFixture(sid: { "c-zz" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.port.heldCallVerbs = ["retractTieBreak"]
        f.store.handle(f.event(.propose(callAudio), sid: "c-aa"))
        await eventually { f.port.callSends.count == 2 }
        f.store.handle(f.event(.reject(reason: .expired, tieBreak: true), sid: "c-zz"))
        f.port.releaseCallSends()
        await eventually { f.port.callSends.count == 3 }
        #expect(
            f.port.callSends == [
                .propose(callPeer.bare, "c-zz", callAudio), .retractTieBreak(callPeer, "c-zz"), .ringing(callPeer.bare, "c-aa"),
            ])
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c-aa", media: callAudio)))
        f.store.clear()
    }

    @Test func retractedMigrationEndsAndTerminatesOldSession() async {
        let f = CallFixture(sid: { "c-old" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio), sid: "c-old"))
        f.port.callSends = []
        f.port.heldCallVerbs = ["proceed"]
        f.store.handle(f.event(.propose(callAudio), sid: "c-new"))
        await eventually { f.port.callSends.count == 2 }
        f.store.handle(f.event(.retract(reason: nil, tieBreak: false), sid: "c-new"))
        f.port.releaseCallSends()
        await eventually { f.port.callSends.count == 3 }
        #expect(f.store.state == .ended(sid: "c-old", reason: .expired))
        #expect(
            f.port.callSends == [
                .migrated(callPeer, "c-old", "c-new"), .proceed(callPeer, "c-new"), .terminate(callPeer, "c-old", .expired),
            ])
        f.store.clear()
    }

    @Test func remotelyConcludedOldSessionKeepsProceededMigration() async {
        let f = CallFixture(sid: { "c-old" }, sessionTimeout: .milliseconds(250))
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio), sid: "c-old"))
        f.port.callSends = []
        f.port.heldCallVerbs = ["proceed"]
        f.store.handle(f.event(.propose(callAudio), sid: "c-new"))
        await eventually { f.port.callSends.count == 2 }
        f.store.handle(f.event(.sessionTerminate(reason: .success), sid: "c-old"))
        f.port.releaseCallSends()
        await eventually {
            if case .incoming = f.store.state { return true }
            return false
        }
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c-new", media: callAudio, isAccepting: true)))
        await eventually { f.port.callSends.count == 3 }
        #expect(f.store.state == .ended(sid: "c-new", reason: .timeout))
        #expect(
            f.port.callSends == [.migrated(callPeer, "c-old", "c-new"), .proceed(callPeer, "c-new"), .finish(callPeer, "c-new", .timeout)])
        f.store.clear()
    }

    @Test func retractDuringTieBreakNeverInstallsDeadRing() async {
        let f = CallFixture(sid: { "c-zz" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.port.heldCallVerbs = ["retractTieBreak"]
        f.store.handle(f.event(.propose(callAudio), sid: "c-aa"))
        await eventually { f.port.callSends.count == 2 }
        f.store.handle(f.event(.retract(reason: nil, tieBreak: false), sid: "c-aa"))
        f.port.releaseCallSends()
        await eventually { f.store.state == .ended(sid: "c-zz", reason: .expired) }
        #expect(f.store.state == .ended(sid: "c-zz", reason: .expired))
        #expect(f.port.callSends == [.propose(callPeer.bare, "c-zz", callAudio), .retractTieBreak(callPeer, "c-zz")])
        f.store.clear()
    }

    @Test func hangUpAcceptingRingFinishesWithCancel() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        #expect(await f.store.acceptIncoming())
        f.port.callSends = []
        await f.store.hangUp()
        #expect(f.store.state == .idle)
        #expect(f.port.callSends == [.finish(callPeer, "c1", .cancel)])
    }

    @Test func hangUpDuringMigrationFinishesProceededSIDWithCancel() async {
        let f = CallFixture(sid: { "c-old" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio), sid: "c-old"))
        f.port.callSends = []
        f.port.heldCallVerbs = ["proceed"]
        f.store.handle(f.event(.propose(callAudio), sid: "c-new"))
        await eventually { f.port.callSends.count == 2 }
        await f.store.hangUp()
        f.port.releaseCallSends()
        await eventually { f.port.callSends.count == 5 }
        #expect(f.store.state == .idle)
        #expect(
            f.port.callSends == [
                .migrated(callPeer, "c-old", "c-new"), .proceed(callPeer, "c-new"), .terminate(callPeer, "c-old", .success),
                .finish(callPeer, "c-old", nil), .finish(callPeer, "c-new", .cancel),
            ])
        f.store.clear()
    }

    @Test func decliningAcceptingRingUsesFinishCancel() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        #expect(await f.store.acceptIncoming())
        f.port.callSends = []
        #expect(await f.store.declineIncoming())
        #expect(f.store.state == .idle)
        #expect(f.port.callSends == [.finish(callPeer, "c1", .cancel)])
        f.store.clear()
    }

    @Test func queuedTieBreakAfterLocalHangUpDeclinesPeer() async {
        let f = CallFixture(sid: { "c-zz" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.propose(callAudio), sid: "c-aa"))
        await f.store.hangUp()
        await eventually { f.port.callSends.count == 3 }
        #expect(f.store.state == .idle)
        #expect(
            f.port.callSends == [
                .propose(callPeer.bare, "c-zz", callAudio), .retract(callPeer.bare, "c-zz"), .reject(callPeer, "c-aa", false),
            ])
        f.store.clear()
    }

    @Test func staleProceedCannotTouchANewerCall() async {
        var number = 0
        let f = CallFixture(sid: {
            number += 1
            return number == 1 ? "c-first" : "c-second"
        })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.proceed, sid: "c-first"))
        await f.store.hangUp()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        // Busy response is ordered after the stale proceed effect.
        let carol = JID(parsing: "carol@waddle.test/tab")!
        f.store.handle(f.event(.propose(callAudio), sid: "c-third", from: carol))
        await eventually { f.port.callSends.count == 4 }
        #expect(
            f.port.callSends == [
                .propose(callPeer.bare, "c-first", callAudio), .retract(callPeer.bare, "c-first"),
                .propose(callPeer.bare, "c-second", callAudio), .reject(carol, "c-third", false),
            ])
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c-second", media: callAudio, initiator: callOwn)))
        f.store.clear()
    }

    @Test func noResourceRetractsLosingOursThenDeclinesWinner() async {
        let f = CallFixture(sid: { "c-zz" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.own = nil
        f.store.handle(f.event(.propose(callAudio), sid: "c-aa"))
        await eventually { f.port.callSends.count == 3 }
        #expect(
            f.port.callSends == [
                .propose(callPeer.bare, "c-zz", callAudio), .retractTieBreak(callPeer, "c-zz"), .reject(callPeer, "c-aa", false),
            ])
        #expect(f.store.state == .ended(sid: "c-zz", reason: .expired))
        f.store.clear()
    }

    @Test func noResourceStillRejectsLosingIncoming() async {
        let f = CallFixture(sid: { "c-aa" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.own = nil
        f.store.handle(f.event(.propose(callAudio), sid: "c-zz"))
        await eventually { f.port.callSends.count == 2 }
        #expect(f.port.callSends == [.propose(callPeer.bare, "c-aa", callAudio), .reject(callPeer, "c-zz", true)])
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c-aa", media: callAudio, initiator: callOwn)))
        f.store.clear()
    }

    @Test func abortHistoryRetainsOnlyTheLatest256SIDs() async {
        let f = CallFixture(sid: { "c-zz" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        for number in 0..<257 {
            f.store.handle(f.event(.retract(reason: nil, tieBreak: false), sid: "a\(number)"))
        }
        f.store.handle(f.event(.propose(callAudio), sid: "a0"))
        await eventually { f.store.state.isIncoming }
        f.store.handle(f.event(.propose(callAudio), sid: "a1"))
        let carol = JID(parsing: "carol@waddle.test/tab")!
        f.store.handle(f.event(.propose(callAudio), sid: "c-barrier", from: carol))
        await eventually { f.port.callSends.count == 3 }
        #expect(
            f.port.callSends == [
                .propose(callPeer.bare, "c-zz", callAudio), .retractTieBreak(callPeer, "c-zz"),
                .reject(carol, "c-barrier", false),
            ])
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "a0", media: callAudio)))
        f.store.clear()
    }

}
