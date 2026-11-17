import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@Suite("Call alerts") struct CallAlertTests {
    private let peer = JID(parsing: "bob@waddle.test/phone")!
    private let audio = CallMedia(audio: true, video: false)

    private func alert(_ state: CallState, doNotDisturb: Bool = false) -> CallAlert? {
        CallAlert(state: state, isDoNotDisturb: doNotDisturb)
    }

    @Test func anIncomingCallRingsWithWhoIsCallingAndHow() {
        let state = CallState.incoming(IncomingCall(from: peer, sid: "s1", media: CallMedia(audio: true, video: true)))
        #expect(alert(state) == .ring(.init(sid: "s1", from: peer.bare, isVideo: true)))
    }

    @Test func aCallBeingAnsweredHasStoppedRinging() {
        #expect(alert(.incoming(IncomingCall(from: peer, sid: "s1", media: audio, isAccepting: true))) == nil)
    }

    @Test func doNotDisturbSilencesTheRingButNotOurOwnCall() {
        #expect(alert(.incoming(IncomingCall(from: peer, sid: "s1", media: audio)), doNotDisturb: true) == nil)
        let ringing = CallState.outgoing(OutgoingCall(to: peer.bare, sid: "s2", media: audio, isRinging: true))
        #expect(alert(ringing, doNotDisturb: true) == .ringback(sid: "s2"))
    }

    @Test func ringbackPlaysOnlyWhileTheOtherSideIsRinging() {
        // Until a device of theirs says it is ringing there is nothing to echo.
        #expect(alert(.outgoing(OutgoingCall(to: peer.bare, sid: "s2", media: audio))) == nil)
        #expect(alert(.outgoing(OutgoingCall(to: peer.bare, sid: "s2", media: audio, isRinging: true))) == .ringback(sid: "s2"))
        // They picked up: the session is being set up.
        #expect(alert(.outgoing(OutgoingCall(to: peer.bare, sid: "s2", media: audio, isRinging: true, responder: peer))) == nil)
    }

    @Test func nothingElseMakesASound() {
        #expect(alert(.idle) == nil)
        #expect(alert(.ended(sid: "s1", reason: .rejected)) == nil)
        #expect(alert(.joiningGroup(GroupCallAttempt(
            room: BareJID(parsing: "room@muc.waddle.test")!, sid: "g1", media: audio, nick: "alice"))) == nil)
    }
}

@MainActor @Suite("Unanswered calls") struct UnansweredCallTests {
    @Test func aCallNobodyAnswersStopsRingingByItself() async {
        // The caller's retract never arrived: they lost their connection.
        let f = CallFixture(unansweredTimeout: .milliseconds(40))
        f.store.handle(f.event(.propose(callAudio)))
        #expect(f.store.state.isLive)
        await eventually { f.store.state == .ended(sid: "c1", reason: .missed) }
        // There is nobody to tell.
        #expect(!f.port.callSends.contains { if case .reject = $0 { true } else { false } })
        f.store.clear()
    }

    @Test func anAnsweredCallIsNotCutShortByTheRingRunningOut() async {
        let f = CallFixture(unansweredTimeout: .milliseconds(40))
        f.store.handle(f.event(.propose(callAudio)))
        #expect(await f.store.acceptIncoming())
        try? await Task.sleep(for: .milliseconds(120))
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio, isAccepting: true)))
        f.store.clear()
    }

    @Test func aDeclinedCallLeavesNoTimerBehindForTheNextOne() async {
        let f = CallFixture(unansweredTimeout: .milliseconds(80))
        f.store.handle(f.event(.propose(callAudio)))
        #expect(await f.store.declineIncoming())
        f.store.handle(f.event(.propose(callAudio), sid: "c2"))
        try? await Task.sleep(for: .milliseconds(40))
        #expect(f.store.state.sid == "c2" && f.store.state.isLive)
        await eventually { f.store.state == .ended(sid: "c2", reason: .missed) }
        f.store.clear()
    }
}

extension UnansweredCallTests {
    @Test func aCallThatWonATieBreakAlsoStopsRingingByItself() async {
        let f = CallFixture(sid: { "c-zz" }, unansweredTimeout: .milliseconds(60))
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.propose(callAudio), sid: "c-aa"))
        await eventually { f.store.state.isIncoming }
        await eventually { f.store.state == .ended(sid: "c-aa", reason: .missed) }
        f.store.clear()
    }

    @Test func anAcceptThatFailsLateStillLeavesTheRingBounded() async {
        let f = CallFixture(unansweredTimeout: .milliseconds(60))
        f.store.handle(f.event(.propose(callAudio)))
        // The proceed is still on its way when the ring would have run out.
        f.port.heldCallVerbs = ["proceed"]
        f.port.callResults["proceed"] = false
        let accepting = Task { await f.store.acceptIncoming() }
        try? await Task.sleep(for: .milliseconds(120))
        #expect(f.store.state.isLive)
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await !accepting.value)
        await eventually { f.store.state == .ended(sid: "c1", reason: .missed) }
        f.store.clear()
    }

    @Test func aMissedCallSaysSo() {
        #expect(CallEndReason.missed.summary == "Missed call")
    }
}

@Suite("Call ring plan") struct CallRingPlanTests {
    private let ring = CallAlert.ring(.init(sid: "s1", from: BareJID(parsing: "bob@waddle.test")!, isVideo: false))

    @Test func inFrontARingIsOnlyHeard() {
        #expect(CallRingPlan(alert: ring, isAppActive: true, ringsInBackground: true) == .init(tone: .ring, notifies: false))
        #expect(CallRingPlan(alert: ring, isAppActive: true, ringsInBackground: false) == .init(tone: .ring, notifies: false))
    }

    @Test func behindAnotherAppAMacRingsAndNotifies() {
        #expect(CallRingPlan(alert: ring, isAppActive: false, ringsInBackground: true) == .init(tone: .ring, notifies: true))
    }

    @Test func inTheBackgroundAPhoneLeavesTheRingToItsNotification() {
        #expect(CallRingPlan(alert: ring, isAppActive: false, ringsInBackground: false) == .init(tone: nil, notifies: true))
    }

    @Test func ringbackIsHeardWhereverTheAppIs() {
        for active in [true, false] {
            #expect(CallRingPlan(alert: .ringback(sid: "s2"), isAppActive: active, ringsInBackground: false)
                == .init(tone: .ringback, notifies: false))
        }
    }

    @Test func noAlertIsSilence() {
        #expect(CallRingPlan(alert: nil, isAppActive: false, ringsInBackground: true) == .init(tone: nil, notifies: false))
    }
}
