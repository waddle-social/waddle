import Testing

@testable import WaddleKit

@Suite("Call stage") struct CallStageTests {
    @Test func anIdleSlotShowsNothing() {
        #expect(CallStage(state: .idle, media: .idle) == nil)
    }

    @Test func anIncomingInvitationRingsHereWithItsMediaAndAnswerProgress() {
        let state = CallState.incoming(IncomingCall(from: callPeer, sid: "c1", media: callVideo, isAccepting: true))
        #expect(CallStage(state: state, media: .idle) == .incoming(from: callPeer, media: callVideo, isAccepting: true))
    }

    @Test func aProposalNoDeviceHasAcknowledgedIsCalling() {
        let state = CallState.outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callAudio))
        #expect(CallStage(state: state, media: .idle) == .calling(to: callPeer.bare, media: callAudio))
    }

    @Test func aProposalAPeerDeviceRingsForIsRinging() {
        let state = CallState.outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callAudio, isRinging: true))
        #expect(CallStage(state: state, media: .idle) == .ringing(to: callPeer.bare, media: callAudio))
    }

    @Test func anAnsweredProposalKeepsRingingUntilTheSessionIsAccepted() {
        // A `<proceed/>` can arrive without a `<ringing/>` before it.
        let state = CallState.outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callVideo, responder: callPeer))
        #expect(CallStage(state: state, media: .idle) == .ringing(to: callPeer.bare, media: callVideo))
    }

    /// The store hands the session to media a moment after it goes active,
    /// so media that has not started yet is still connecting.
    @Test(arguments: [CallMediaPhase.idle, .connecting])
    func anEstablishedSessionWhoseMediaIsNotUpIsConnecting(phase: CallMediaPhase) {
        #expect(CallStage(state: active, media: phase) == .connecting(peer: callPeer.bare, media: callVideo))
    }

    @Test func anEstablishedSessionWithConnectedMediaIsInCall() {
        #expect(CallStage(state: active, media: .connected) == .inCall(peer: callPeer.bare, media: callVideo))
    }

    @Test func anEstablishedSessionWhoseMediaIsReconnectingIsReconnecting() {
        #expect(CallStage(state: active, media: .reconnecting) == .reconnecting(peer: callPeer.bare, media: callVideo))
    }

    @Test func anEndedSlotShowsWhyRegardlessOfMedia() {
        #expect(CallStage(state: .ended(sid: "c1", reason: .timeout), media: .connected) == .ended(.timeout))
    }

    @Test func theIncomingPeerIsTheProposersAccount() {
        #expect(CallStage.incoming(from: callPeer, media: callAudio, isAccepting: false).peer == callPeer.bare)
    }

    @Test func anEndedStageNamesNoPeer() {
        #expect(CallStage.ended(.timeout).peer == nil)
    }

    @Test(arguments: [
        CallStage.connecting(peer: callPeer.bare, media: callAudio),
        .inCall(peer: callPeer.bare, media: callAudio),
        .reconnecting(peer: callPeer.bare, media: callAudio),
    ])
    func anEstablishedSessionIsInSession(stage: CallStage) {
        #expect(stage.isInSession)
    }

    @Test(arguments: [
        CallStage.incoming(from: callPeer, media: callAudio, isAccepting: true),
        .calling(to: callPeer.bare, media: callAudio),
        .ringing(to: callPeer.bare, media: callAudio),
        .ended(.timeout),
    ])
    func aRingOrAnEndedCallIsNotInSession(stage: CallStage) {
        #expect(!stage.isInSession)
    }

    private var active: CallState {
        .active(ActiveCall(peer: callPeer, sid: "c1", media: callVideo, join: callJoin))
    }
}

@Suite("Call end summary") struct CallEndSummaryTests {
    @Test func aRejectedInvitationWasDeclined() {
        #expect(CallEndReason.rejected.summary == "Call declined")
    }

    @Test func aRetractedInvitationWasCancelled() {
        #expect(CallEndReason.retracted.summary == "Call cancelled")
    }

    /// A lost XEP-0353 tie-break: the other proposal became the call.
    @Test func anExpiredProposalWasAnsweredElsewhere() {
        #expect(CallEndReason.expired.summary == "Answered on another device")
    }

    @Test func aTimedOutRingHadNoAnswer() {
        #expect(CallEndReason.timeout.summary == "No answer")
    }

    @Test func aFailedSendCouldNotConnect() {
        #expect(CallEndReason.error.summary == "Call could not connect")
    }

    @Test(arguments: [nil, JingleReason.success])
    func aSessionFinishedNormallyEnded(reason: JingleReason?) {
        #expect(CallEndReason.finished(reason).summary == "Call ended")
    }

    @Test func aSessionTheyDeclinedWasDeclined() {
        #expect(CallEndReason.finished(.decline).summary == "Call declined")
    }

    @Test func aBusyPeerIsBusy() {
        #expect(CallEndReason.finished(.busy).summary == "Busy right now")
    }

    @Test func aCancelledSessionWasCancelled() {
        #expect(CallEndReason.finished(.cancel).summary == "Call cancelled")
    }

    @Test(arguments: [JingleReason.timeout, .expired])
    func aSessionThatRanOutOfTimeHadNoAnswer(reason: JingleReason) {
        #expect(CallEndReason.finished(reason).summary == "No answer")
    }

    @Test func anAlternativeSessionWasAnsweredElsewhere() {
        #expect(CallEndReason.finished(.alternativeSession).summary == "Answered on another device")
    }

    @Test(arguments: [JingleReason.connectivityError, .failedTransport, .mediaError, .gone])
    func aSessionWhoseMediaDiedLostTheConnection(reason: JingleReason) {
        #expect(CallEndReason.finished(reason).summary == "Connection lost")
    }

    @Test(arguments: [
        JingleReason.failedApplication, .generalError, .incompatibleParameters, .securityError,
        .unsupportedApplications, .unsupportedTransports,
    ])
    func aSessionTheDevicesCouldNotAgreeOnCouldNotConnect(reason: JingleReason) {
        #expect(CallEndReason.finished(reason).summary == "Call could not connect")
    }
}

@Suite("Call clock") struct CallClockTests {
    @Test func aCallUnderAMinuteShowsMinutesAndPaddedSeconds() {
        #expect(CallClock.string(for: .seconds(7)) == "0:07")
    }

    @Test func minutesAreNotPadded() {
        #expect(CallClock.string(for: .seconds(12 * 60 + 34)) == "12:34")
    }

    @Test func anHourOrMoreShowsHoursAndPaddedMinutes() {
        #expect(CallClock.string(for: .seconds(3600 + 2 * 60 + 5)) == "1:02:05")
    }

    @Test func aPartSecondIsNotCountedYet() {
        #expect(CallClock.string(for: .milliseconds(59_999)) == "0:59")
    }
}

struct CallSessionStatusTests {
    @Test(arguments: [
        (CallMediaPhase.idle, CallSessionStatus.connecting), (.connecting, .connecting), (.connected, .connected),
        (.reconnecting, .reconnecting),
    ])
    func theMediaPhaseIsReadAsAStatusInOnePlace(_ phase: CallMediaPhase, _ status: CallSessionStatus) {
        #expect(CallSessionStatus(phase) == status)
    }

    @Test func theDevicesCanBeSwitchedOnceMediaHasConnected() {
        #expect(!CallSessionStatus.connecting.hasMedia)
        #expect(CallSessionStatus.connected.hasMedia)
        #expect(CallSessionStatus.reconnecting.hasMedia, "a call that is reconnecting has had media")
    }

    @Test func eachStatusIsSaid() {
        #expect(CallSessionStatus.allCases.map(\.announcement) == ["Connecting", "Call connected", "Reconnecting"])
    }
}
