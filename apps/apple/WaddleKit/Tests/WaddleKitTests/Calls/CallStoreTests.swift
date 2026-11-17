import Foundation
import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Call store") struct CallStoreTests {
    @Test func proceedBeatsARingTimeoutThatIsAlreadyDue() async {
        let f = CallFixture(ringTimeout: .milliseconds(50))
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        // The deadline passes while the main actor is busy, so the timer
        // and the peer's proceed are both due; the proceed is handled first.
        holdMainActor(for: 0.2)
        f.store.handle(f.event(.proceed))
        await eventually { f.port.callSends.count == 2 }
        try? await Task.sleep(for: .milliseconds(50))
        #expect(f.port.callSends == [.propose(callPeer.bare, "c1", callAudio), .initiate(callPeer, callOwn, "c1", callAudio)])
        #expect(
            f.store.state
                == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callAudio, initiator: callOwn, responder: callPeer)))
        f.store.clear()
    }

    @Test func sessionInitiateBeforeLocalAcceptIsIgnored() async {
        let media = RecordingCallMedia()
        let port = FakePort()
        let store = CallStore(port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare, media: media)
        store.handle(CallEvent(from: callPeer, sid: "c1", kind: .propose(callAudio)))
        store.handle(CallEvent(from: callPeer, sid: "c1", kind: .sessionInitiate(join: callJoin, media: callAudio)))
        await eventually { port.completedCallSends.count == 1 }
        try? await Task.sleep(for: .milliseconds(50))
        #expect(store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        #expect(port.callSends == [.ringing(callPeer.bare, "c1")])
        #expect(media.calls.isEmpty)
        store.clear()
    }

    @Test func proposeCompletingAfterProceedDoesNotRearmTheRingTimer() async {
        let f = CallFixture(ringTimeout: .milliseconds(50))
        f.port.heldCallVerbs = ["propose"]
        let starting = Task { await f.store.startCall(to: callPeer.bare, media: callAudio) }
        await eventually { f.port.callSends.count == 1 }
        f.store.handle(f.event(.proceed))
        await eventually { f.port.callSends.count == 2 }
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await starting.value)
        try? await Task.sleep(for: .milliseconds(200))
        #expect(f.port.callSends == [.propose(callPeer.bare, "c1", callAudio), .initiate(callPeer, callOwn, "c1", callAudio)])
        #expect(
            f.store.state
                == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callAudio, initiator: callOwn, responder: callPeer)))
        f.store.clear()
    }

    @Test func onlyTheFirstAnsweringDeviceGetsTheSession() async {
        let f = CallFixture()
        let tablet = JID(parsing: "bob@waddle.test/tablet")!
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.proceed))
        f.store.handle(f.event(.proceed, from: tablet))
        await eventually { f.port.completedCallSends.count == 2 }
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio), from: tablet))
        try? await Task.sleep(for: .milliseconds(50))
        #expect(f.port.callSends == [.propose(callPeer.bare, "c1", callAudio), .initiate(callPeer, callOwn, "c1", callAudio)])
        #expect(
            f.store.state
                == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callAudio, initiator: callOwn, responder: callPeer)))
        f.store.clear()
    }

    @Test func callerCannotAddVideoToAnAcceptedAudioInvitation() async {
        let media = RecordingCallMedia()
        let port = FakePort()
        let store = CallStore(port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare, media: media)
        store.handle(CallEvent(from: callPeer, sid: "c1", kind: .propose(callAudio)))
        await eventually { port.callSends.count == 1 }
        #expect(await store.acceptIncoming())
        store.handle(CallEvent(from: callPeer, sid: "c1", kind: .sessionInitiate(join: callJoin, media: callVideo)))
        await eventually { port.callSends.count == 3 && media.calls.count == 1 }
        #expect(store.state == .active(ActiveCall(peer: callPeer, sid: "c1", media: callAudio, join: callJoin, initiator: callPeer)))
        #expect(port.callSends.last == .accept(callPeer, callOwn, "c1", callAudio))
        #expect(media.calls == [.connect(callJoin, callAudio)])
        store.clear()
    }

    @Test func responderCannotAddVideoToAnAudioCallWePlaced() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.proceed))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callVideo)))
        #expect(f.store.state == .active(ActiveCall(peer: callPeer, sid: "c1", media: callAudio, join: callJoin, initiator: callOwn)))
        f.store.clear()
    }

    @Test func anotherOfOurDevicesFinishingTheCallStopsThisOneRinging() async {
        let f = CallFixture()
        let ownTablet = JID(parsing: "alice@waddle.test/tablet")!
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        f.store.handle(f.event(.finish(reason: .success, migratedTo: nil), from: ownTablet))
        #expect(f.store.state == .ended(sid: "c1", reason: .finished(.success)))
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1")])
        f.store.clear()
    }

    @Test func peerTerminatingTheSessionIsAnsweredWithOurFinish() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        f.port.callSends = []
        f.store.handle(f.event(.sessionTerminate(reason: .success)))
        // The peer's own finish follows; it must not draw a second one.
        f.store.handle(f.event(.finish(reason: .success, migratedTo: nil)))
        await eventually { f.port.completedCallSends.count == 2 }
        try? await Task.sleep(for: .milliseconds(50))
        #expect(f.port.callSends == [.finish(callPeer, "c1", .success)])
        #expect(f.store.state == .ended(sid: "c1", reason: .finished(.success)))
        f.store.clear()
    }

    @Test func rejectFromADeviceWeDidNotCallCannotEndTheCall() async {
        let f = CallFixture()
        let tablet = JID(parsing: "bob@waddle.test/tablet")!
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.proceed))
        f.store.handle(f.event(.reject(reason: nil, tieBreak: false), from: tablet))
        #expect(
            f.store.state
                == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callAudio, initiator: callOwn, responder: callPeer)))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        f.store.handle(f.event(.reject(reason: nil, tieBreak: false), from: tablet))
        f.store.handle(f.event(.retract(reason: nil, tieBreak: false)))
        #expect(f.store.state == .active(ActiveCall(peer: callPeer, sid: "c1", media: callAudio, join: callJoin, initiator: callOwn)))
        f.store.clear()
    }

    @Test func redeliveredProposeForTheActiveCallChangesNothing() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        #expect(await f.store.acceptIncoming())
        f.store.handle(f.event(.sessionInitiate(join: callJoin, media: callAudio)))
        await eventually { f.port.completedCallSends.count == 3 }
        f.store.handle(f.event(.propose(callAudio)))
        let carol = JID(parsing: "carol@waddle.test/tab")!
        f.store.handle(f.event(.propose(callAudio), sid: "c-barrier", from: carol))
        await eventually { f.port.callSends.last == .reject(carol, "c-barrier", false) }
        #expect(
            f.port.callSends == [
                .ringing(callPeer.bare, "c1"), .proceed(callPeer, "c1"), .accept(callPeer, callOwn, "c1", callAudio),
                .reject(carol, "c-barrier", false),
            ])
        #expect(f.store.state == .active(ActiveCall(peer: callPeer, sid: "c1", media: callAudio, join: callJoin, initiator: callPeer)))
        f.store.clear()
    }

    @Test func redeliveredProposeForAnEndedCallDoesNotRingAgain() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        f.store.handle(f.event(.retract(reason: nil, tieBreak: false)))
        f.store.handle(f.event(.propose(callAudio)))
        try? await Task.sleep(for: .milliseconds(50))
        #expect(f.store.state == .ended(sid: "c1", reason: .retracted))
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1")])
        f.store.clear()
    }

    @Test func onlyTheCallerCanStartTheSessionOrStopTheRing() async {
        let f = CallFixture()
        let carol = JID(parsing: "carol@waddle.test/tab")!
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        // A proceed is how one of OUR devices answers; the caller cannot send it.
        f.store.handle(f.event(.proceed))
        f.store.handle(f.event(.retract(reason: nil, tieBreak: false), from: carol))
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        #expect(await f.store.acceptIncoming())
        f.store.handle(f.event(.sessionInitiate(join: callJoin, media: callAudio), from: carol))
        try? await Task.sleep(for: .milliseconds(50))
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio, isAccepting: true)))
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1"), .proceed(callPeer, "c1")])
        f.store.clear()
    }

    @Test func hangingUpAnAnsweredCallBeforeItConnectsTerminatesAndFinishes() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.proceed))
        await eventually { f.port.completedCallSends.count == 2 }
        // The server may not know the session yet; the finish goes out regardless.
        f.port.callTerminateOutcome = .error
        await f.store.hangUp()
        #expect(
            f.port.callSends == [
                .propose(callPeer.bare, "c1", callAudio), .initiate(callPeer, callOwn, "c1", callAudio),
                .terminate(callPeer, "c1", .success), .finish(callPeer, "c1", nil),
            ])
        #expect(f.store.state == .idle)
        f.store.clear()
    }

    @Test func proceedFromSomeoneWeDidNotCallLeavesTheRingTimeoutRunning() async {
        let f = CallFixture(ringTimeout: .milliseconds(50))
        let carol = JID(parsing: "carol@waddle.test/tab")!
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.proceed, from: carol))
        await eventually { f.port.callSends.count == 2 }
        #expect(f.port.callSends == [.propose(callPeer.bare, "c1", callAudio), .retract(callPeer.bare, "c1")])
        #expect(f.store.state == .ended(sid: "c1", reason: .timeout))
        f.store.clear()
    }

    @Test func mediaFailureEndsTheCallWithItsReasonOnShow() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.proceed))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        await eventually { f.port.completedCallSends.count == 2 }
        f.port.callSends = []
        await f.store.hangUpActive(ifSid: "c1", reason: .connectivityError)
        // The person sees why the call went away instead of it vanishing.
        #expect(f.store.state == .ended(sid: "c1", reason: .finished(.connectivityError)))
        #expect(f.port.callSends == [.terminate(callPeer, "c1", .connectivityError), .finish(callPeer, "c1", nil)])
        f.store.clear()
    }

    @Test func declinedInvitationIsNotRungAfterwards() async {
        let f = CallFixture()
        let dave = JID(parsing: "dave@waddle.test/tab")!
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        #expect(await f.store.acceptIncoming())
        // The session-accept send is held, so later effects queue behind it.
        f.port.heldCallVerbs = ["accept"]
        f.store.handle(f.event(.sessionInitiate(join: callJoin, media: callAudio)))
        await eventually { f.port.callSends.count == 3 }
        f.store.handle(f.event(.sessionTerminate(reason: .success)))
        f.store.handle(f.event(.propose(callAudio), sid: "c3", from: dave))
        #expect(await f.store.declineIncoming())
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        await eventually { f.port.completedCallSends.count == 5 }
        try? await Task.sleep(for: .milliseconds(50))
        #expect(
            f.port.callSends == [
                .ringing(callPeer.bare, "c1"), .proceed(callPeer, "c1"), .accept(callPeer, callOwn, "c1", callAudio),
                .reject(dave, "c3", false), .finish(callPeer, "c1", .success),
            ])
        #expect(f.store.state == .idle)
        f.store.clear()
    }

    @Test func incomingProposeRingsTheCallersBareJID() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callVideo)))
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callVideo)))
        await eventually { f.port.callSends.count == 1 }
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1")])
        f.store.clear()
    }
    @Test func duplicateProposeDoesNotRingTwice() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        f.store.handle(f.event(.propose(callAudio)))
        try? await Task.sleep(for: .milliseconds(50))
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1")])
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        f.store.clear()
    }

    @Test func acceptingSendsProceedToFullJID() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        #expect(await f.store.acceptIncoming())
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio, isAccepting: true)))
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1"), .proceed(callPeer, "c1")])
        f.store.clear()
    }

    @Test func decliningRejectsAndClears() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        #expect(await f.store.declineIncoming())
        #expect(f.store.state == .idle)
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1"), .reject(callPeer, "c1", false)])
    }

    @Test func sessionInitiateEstablishesAndConfirmsAsOurResource() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        #expect(await f.store.acceptIncoming())
        f.store.handle(f.event(.sessionInitiate(join: callJoin, media: callAudio)))
        #expect(f.store.state == .active(ActiveCall(peer: callPeer, sid: "c1", media: callAudio, join: callJoin, initiator: callPeer)))
        await eventually { f.port.callSends.count == 3 }
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1"), .proceed(callPeer, "c1"), .accept(callPeer, callOwn, "c1", callAudio)])
        f.store.clear()
    }

    @Test func staleSessionInitiateIsIgnored() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        f.store.handle(f.event(.sessionInitiate(join: callJoin, media: callAudio), sid: "other-sid"))
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1")])
        f.store.clear()
    }

    @Test func startProposesToBarePeerAndRecordsInitiator() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callAudio, initiator: callOwn)))
        #expect(f.port.callSends == [.propose(callPeer.bare, "c1", callAudio)])
        f.store.clear()
    }

    @Test func startIsRefusedWhenAnotherCallIsLive() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        #expect(await !f.store.startCall(to: callPeer.bare, media: callAudio))
        await eventually { f.port.callSends.count == 1 }
        #expect(f.port.callSends == [.ringing(callPeer.bare, "c1")])
        f.store.clear()
    }

    @Test func aCallThatCouldNotBeProposedEndsWithAnErrorOnShow() async {
        let f = CallFixture()
        f.port.callResults["propose"] = false
        #expect(await !f.store.startCall(to: callPeer.bare, media: callAudio))
        // Tapping Call must not look like nothing happened.
        #expect(f.store.state == .ended(sid: "c1", reason: .error))
        #expect(f.store.lastError == "call propose failed")
    }

    @Test func ringingFromCalleeMarksOutgoingSlot() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.ringing))
        #expect(
            f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callAudio, initiator: callOwn, isRinging: true)))
        f.store.clear()
    }

    @Test func proceedInitiatesAsOurResourceAndTimesOutWaitingForAccept() async {
        let port = FakePort()
        let store = CallStore(
            port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare,
            ringTimeout: .seconds(10), sessionTimeout: .milliseconds(30), newSid: { "c-out" })
        #expect(await store.startCall(to: callPeer.bare, media: callVideo))
        store.handle(CallEvent(from: callPeer, sid: "c-out", kind: .proceed))
        await eventually { port.callSends.count == 4 }
        // The finish reaches the peer's other devices; the terminate IQ does not.
        #expect(
            port.callSends == [
                .propose(callPeer.bare, "c-out", callVideo), .initiate(callPeer, callOwn, "c-out", callVideo),
                .terminate(callPeer, "c-out", .timeout), .finish(callPeer, "c-out", .timeout),
            ])
        #expect(store.state == .ended(sid: "c-out", reason: .timeout))
        store.clear()
    }

    @Test func sessionAcceptActivatesWithOurInitiatorAndRetiresTimeout() async {
        let f = CallFixture(sessionTimeout: .milliseconds(100))
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.proceed))
        await eventually { f.port.callSends.count == 2 }
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        let clock = CallFixture(ringTimeout: .milliseconds(200))
        #expect(await clock.store.startCall(to: callPeer.bare, media: callAudio))
        await eventually { clock.port.callSends.count == 2 }
        #expect(f.store.state == .active(ActiveCall(peer: callPeer, sid: "c1", media: callAudio, join: callJoin, initiator: callOwn)))
        #expect(f.port.callSends == [.propose(callPeer.bare, "c1", callAudio), .initiate(callPeer, callOwn, "c1", callAudio)])
        f.store.clear()
        clock.store.clear()
    }

    @Test func unansweredOutgoingAutomaticallyRetracts() async {
        let port = FakePort()
        let store = CallStore(
            port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare,
            ringTimeout: .milliseconds(30), newSid: { "c-out" })
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        await eventually { port.callSends.count == 2 }
        #expect(store.state == .ended(sid: "c-out", reason: .timeout))
        #expect(port.callSends == [.propose(callPeer.bare, "c-out", callAudio), .retract(callPeer.bare, "c-out")])
        store.clear()
    }

    @Test func peerRejectEndsOutgoingAndCancelsRing() async {
        let f = CallFixture(ringTimeout: .milliseconds(30))
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.reject(reason: nil, tieBreak: false)))
        let clock = CallFixture(ringTimeout: .milliseconds(60))
        #expect(await clock.store.startCall(to: callPeer.bare, media: callAudio))
        await eventually { clock.port.callSends.count == 2 }
        #expect(f.store.state == .ended(sid: "c1", reason: .rejected))
        #expect(f.port.callSends == [.propose(callPeer.bare, "c1", callAudio)])
        f.store.clear()
        clock.store.clear()
    }

    @Test func remoteTerminateEndsActiveWithReason() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        f.store.handle(f.event(.sessionTerminate(reason: .success)))
        #expect(f.store.state == .ended(sid: "c1", reason: .finished(.success)))
        f.store.clear()
    }

    @Test func remoteFinishEndsIncomingWithReason() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        f.store.handle(f.event(.finish(reason: .success, migratedTo: nil)))
        #expect(f.store.state == .ended(sid: "c1", reason: .finished(.success)))
        f.store.clear()
    }

    @Test func activeHangUpTerminatesThenFinishes() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        f.port.callSends = []
        await f.store.hangUp()
        #expect(f.store.state == .idle)
        #expect(f.port.callSends == [.terminate(callPeer, "c1", .success), .finish(callPeer, "c1", nil)])
    }

    @Test func orphanedTerminateStillGetsFinish() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        f.port.callSends = []
        f.port.callTerminateOutcome = .orphaned
        await f.store.hangUp()
        #expect(f.port.callSends == [.terminate(callPeer, "c1", .success), .finish(callPeer, "c1", nil)])
    }

    @Test func failedTerminateSkipsFinish() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        f.port.callSends = []
        f.port.callTerminateOutcome = .error
        await f.store.hangUp()
        #expect(f.port.callSends == [.terminate(callPeer, "c1", .success)])
        #expect(f.store.lastError == "call session terminate failed")
    }

    @Test func ownProposeCarbonNeverRings() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio), from: callOwn))
        #expect(f.store.state == .idle)
        #expect(f.port.callSends.isEmpty)
        f.store.clear()
    }

    @Test func outgoingHangUpRetracts() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.port.callSends = []
        await f.store.hangUp()
        #expect(f.store.state == .idle)
        #expect(f.port.callSends == [.retract(callPeer.bare, "c1")])
    }

    @Test func ownProceedCarbonStopsLocalRingWithoutReply() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        f.port.callSends = []
        f.store.handle(f.event(.proceed, from: callOwn))
        #expect(f.store.state == .idle)
        #expect(f.port.callSends.isEmpty)
        f.store.clear()
    }

    @Test func ownRejectCarbonStopsIncomingWithoutReply() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        f.port.callSends = []
        f.store.handle(f.event(.reject(reason: nil, tieBreak: false), from: callOwn))
        #expect(f.store.state == .ended(sid: "c1", reason: .rejected))
        #expect(f.port.callSends.isEmpty)
        f.store.clear()
    }

    @Test func unrelatedOwnCarbonIsIgnored() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        f.store.handle(f.event(.proceed, sid: "unrelated", from: callOwn))
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        f.store.clear()
    }

    @Test func dismissClearsOnlyEndedSlot() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        f.store.handle(f.event(.retract(reason: nil, tieBreak: false)))
        f.store.dismiss()
        #expect(f.store.state == .idle)
        f.store.clear()
    }

    @Test func dismissCannotKillAcceptingIncoming() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        #expect(await f.store.acceptIncoming())
        f.store.dismiss()
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio, isAccepting: true)))
        f.store.clear()
    }

    @Test func aFreshProposeCanReplaceEnded() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        f.store.handle(f.event(.retract(reason: nil, tieBreak: false)))
        f.store.handle(f.event(.propose(callAudio), sid: "c2"))
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c2", media: callAudio)))
        f.store.clear()
    }

    @Test func startCanReplaceEnded() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.reject(reason: nil, tieBreak: false)))
        #expect(await f.store.startCall(to: callPeer.bare, media: callVideo))
        #expect(f.store.state == .outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callVideo, initiator: callOwn)))
        #expect(f.store.lastError == nil)
        f.store.clear()
    }

    @Test func callerRetractEndsIncomingWithRetractedReason() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        f.store.handle(f.event(.retract(reason: nil, tieBreak: false)))
        #expect(f.store.state == .ended(sid: "c1", reason: .retracted))
        f.store.clear()
    }

    @Test func successfulHangUpPreservesLastWireFailure() async {
        let f = CallFixture()
        f.port.callResults["ringing"] = false
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.store.lastError != nil }
        await f.store.hangUp()
        #expect(f.store.state == .idle)
        #expect(f.store.lastError == "call ringing failed")
        f.store.clear()
    }

    @Test func responderTimeoutPreservesEarlierWireFailure() async {
        let f = CallFixture(sessionTimeout: .milliseconds(30))
        f.port.callResults["ringing"] = false
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.store.lastError != nil }
        #expect(await f.store.acceptIncoming())
        await eventually { f.port.callSends.count == 3 }
        #expect(f.store.state == .ended(sid: "c1", reason: .timeout))
        #expect(f.store.lastError == "call ringing failed")
        f.store.clear()
    }

    @Test func mediaHangUpCannotClaimIncomingOrAnotherActiveSID() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { f.port.callSends.count == 1 }
        await f.store.hangUpActive(ifSid: "c1", reason: .gone)
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)))
        #expect(await f.store.acceptIncoming())
        f.store.handle(f.event(.sessionInitiate(join: callJoin, media: callAudio)))
        await eventually { f.port.callSends.count == 3 }
        await f.store.hangUpActive(ifSid: "other-sid", reason: .gone)
        #expect(f.store.state == .active(ActiveCall(peer: callPeer, sid: "c1", media: callAudio, join: callJoin, initiator: callPeer)))
        #expect(
            f.port.callSends == [.ringing(callPeer.bare, "c1"), .proceed(callPeer, "c1"), .accept(callPeer, callOwn, "c1", callAudio)])
        f.store.clear()
    }

}

/// Blocks the calling thread, so main-actor work that falls due meanwhile queues up.
private func holdMainActor(for seconds: TimeInterval) {
    Thread.sleep(forTimeInterval: seconds)
}
