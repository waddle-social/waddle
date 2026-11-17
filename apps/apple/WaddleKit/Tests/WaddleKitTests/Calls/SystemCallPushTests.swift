import Foundation
import Testing
import WaddleKit

@Suite("Pushed system calls") struct SystemCallPushTests {
    private let peer = JID(parsing: "bob@waddle.test/phone")!
    private let own = JID(parsing: "me@waddle.test/mac")!
    private let audio = CallMedia(audio: true, video: false)
    private let id1 = UUID(uuidString: "00000000-0000-0000-0000-000000000001")!
    private let id2 = UUID(uuidString: "00000000-0000-0000-0000-000000000002")!

    private func tracker() -> SystemCallTracker {
        var next = [id2, id1]
        return SystemCallTracker(newID: { next.popLast() ?? UUID() })
    }

    private func incoming(_ sid: String = "s1", accepting: Bool = false) -> CallState {
        .incoming(IncomingCall(from: peer, sid: sid, media: audio, isAccepting: accepting))
    }

    private func outgoing(_ sid: String = "s2") -> CallState {
        .outgoing(OutgoingCall(to: peer.bare, sid: sid, media: audio, initiator: own))
    }

    private func active(_ sid: String = "s1") -> CallState {
        let join = CallJoin(url: "wss://sfu.waddle.test", room: "r", identity: "me@waddle.test/mac", token: "t")
        return .active(ActiveCall(peer: peer, sid: sid, media: audio, join: join))
    }

    private func call(_ id: UUID, sid: String = "s1") -> SystemCall {
        SystemCall(id: id, sid: sid, direction: .incoming, conversation: .direct(peer.bare), isVideo: false)
    }

    private func push(
        _ tracker: inout SystemCallTracker, sid: String = "s1", expired: Bool = false,
        finished: Bool = false, state: CallState = .idle
    ) -> PushedCall {
        tracker.pushed(
            sid: sid, conversation: peer.bare, isVideo: false, isExpired: expired, isFinished: finished, state: state)
    }

    @Test func aPushReportsAndWaitsUntilItsProposalArrives() {
        var t = tracker()
        let result = push(&t)
        #expect(result.verdict == .awaiting)
        #expect(result.updates == [.reportIncoming(call(id1))])
        #expect(t.isAwaitingSlot)
        #expect(t.sid(for: id1) == "s1")
        #expect(t.audioOwner == .system)
        #expect(t.update(state: .idle, isMediaConnected: false).isEmpty)
        #expect(t.update(state: .ended(sid: "old", reason: .missed), isMediaConnected: false).isEmpty)
        #expect(t.isAwaitingSlot)
        #expect(t.update(state: incoming(), isMediaConnected: false).isEmpty)
        #expect(!t.isAwaitingSlot)
        #expect(t.managedCall == call(id1))
    }

    @Test func aStreamProposalThenPushReReportsTheSameCall() {
        var t = tracker()
        _ = t.update(state: incoming(), isMediaConnected: false)
        let result = push(&t, state: incoming())
        #expect(result.verdict == .duplicate)
        #expect(result.updates == [.reportDuplicate(call(id1))])
        #expect(!t.isAwaitingSlot)
        #expect(t.managedCall == call(id1))
        #expect(t.update(state: incoming(), isMediaConnected: false).isEmpty)
        #expect(t.update(state: active(), isMediaConnected: false) == [.answer(id1)])
    }

    @Test func twoPushesKeepTheFirstCallAwaiting() {
        var t = tracker()
        _ = push(&t)
        let result = push(&t)
        #expect(result.verdict == .duplicate)
        #expect(result.updates == [.reportDuplicate(call(id1))])
        #expect(t.isAwaitingSlot)
        #expect(t.update(state: .idle, isMediaConnected: false).isEmpty)
        #expect(t.managedCall == call(id1))
    }

    @Test func aVideoPushKeepsItsMediaAndConversation() {
        var t = tracker()
        let result = t.pushed(
            sid: "video", conversation: peer.bare, isVideo: true, isExpired: false, isFinished: false, state: .idle)
        let expected = SystemCall(
            id: id1, sid: "video", direction: .incoming, conversation: .direct(peer.bare), isVideo: true)
        #expect(result.updates == [.reportIncoming(expected)])
        #expect(t.managedCall == expected)
    }

    @Test func aKnownCallIsStillADuplicateWhenThePushHasExpired() {
        var t = tracker()
        _ = push(&t)
        #expect(push(&t, expired: true, finished: true).updates == [.reportDuplicate(call(id1))])
        #expect(t.isAwaitingSlot)
    }

    @Test(arguments: [(true, false), (false, true)])
    func anExpiredOrFinishedCallGetsADisposableReport(expired: Bool, finished: Bool) {
        var t = tracker()
        let result = push(&t, expired: expired, finished: finished)
        #expect(result.verdict == .dismissed)
        #expect(result.updates == [.reportDismissed(call(id1), .unanswered)])
        #expect(!t.isManaging)
        #expect(t.audioOwner == .nobody)
        #expect(t.update(state: .idle, isMediaConnected: false).isEmpty)
    }

    @Test func anotherManagedCallStaysUntouchedByAPush() {
        var t = tracker()
        _ = t.update(state: incoming("s2"), isMediaConnected: false)
        let before = t.managedCall
        let result = push(&t, state: incoming("s2"))
        #expect(result.verdict == .dismissed)
        #expect(result.updates == [.reportDismissed(call(id2), .unanswered)])
        #expect(t.managedCall == before)
        #expect(t.update(state: incoming("s2"), isMediaConnected: false).isEmpty)
    }

    @Test func anOutgoingSlotDismissesThePushBeforeTheTrackerHasSeenIt() {
        var t = tracker()
        let result = push(&t, state: outgoing())
        #expect(result.verdict == .dismissed)
        #expect(result.updates == [.reportDismissed(call(id1), .unanswered)])
        #expect(!t.isManaging)
        #expect(t.update(state: outgoing(), isMediaConnected: false).count == 1)
    }

    @Test func anOutgoingManagedCallSurvivesADismissedPush() {
        var t = tracker()
        _ = t.update(state: outgoing(), isMediaConnected: false)
        let before = t.managedCall
        #expect(push(&t, state: outgoing()).updates == [.reportDismissed(call(id2), .unanswered)])
        #expect(t.managedCall == before)
        #expect(t.update(state: outgoing(), isMediaConnected: false).isEmpty)
    }

    @Test func anUnmanagedCallWithTheSameSidDismissesThePush() {
        var t = tracker()
        _ = t.update(state: incoming(), isMediaConnected: false, isDoNotDisturb: true)
        #expect(push(&t, state: incoming()).updates == [.reportDismissed(call(id1), .unanswered)])
        #expect(!t.isManaging)
        #expect(t.audioOwner == .app)
        #expect(t.update(state: incoming(), isMediaConnected: false).isEmpty)
    }

    @Test func anEndedSlotWithTheSameSidDoesNotWaitAgain() {
        var t = tracker()
        #expect(push(&t, state: .ended(sid: "s1", reason: .missed)).verdict == .dismissed)
        #expect(!t.isManaging)
    }

    @Test func aSecondCallWhileAwaitingDoesNotReplaceTheFirst() {
        var t = tracker()
        _ = push(&t)
        #expect(push(&t, sid: "s2").updates == [.reportDismissed(call(id2, sid: "s2"), .unanswered)])
        #expect(t.managedCall == call(id1))
        #expect(t.isAwaitingSlot)
    }

    @Test func aPushDeadlineEndsOnlyItsAwaitingCall() {
        var t = tracker()
        _ = push(&t)
        #expect(t.pushExpired(sid: "other").isEmpty)
        #expect(t.isAwaitingSlot)
        #expect(t.pushExpired(sid: "s1") == [.end(id1, .unanswered)])
        #expect(t.pushExpired(sid: "s1").isEmpty)
        #expect(!t.isManaging)
        #expect(t.audioOwner == .nobody)
    }

    @Test func aPushDeadlineDoesNotEndAnAdoptedCall() {
        var t = tracker()
        _ = push(&t)
        _ = t.update(state: incoming(), isMediaConnected: false)
        #expect(t.pushExpired(sid: "s1").isEmpty)
        #expect(t.managedCall == call(id1))
    }

    @Test func aSessionEndingFailsAnAwaitingCallOnly() {
        var t = tracker()
        #expect(t.sessionEnded().isEmpty)
        _ = push(&t)
        #expect(t.sessionEnded() == [.end(id1, .failed)])
        #expect(t.sessionEnded().isEmpty)
        #expect(!t.isManaging)
        _ = t.update(state: incoming("s2"), isMediaConnected: false)
        #expect(t.sessionEnded().isEmpty)
        #expect(t.isManaging)
    }

    @Test func anotherLiveSidEndsTheWaitBeforeReportingItsCall() {
        var t = tracker()
        _ = push(&t)
        #expect(t.update(state: incoming("s2"), isMediaConnected: false) == [
            .end(id1, .unanswered), .reportIncoming(call(id2, sid: "s2")),
        ])
        #expect(!t.isAwaitingSlot)
        #expect(t.managedCall == call(id2, sid: "s2"))
    }

    @Test func adoptionUnderDoNotDisturbLeavesTheCallToTheApp() {
        var t = tracker()
        _ = push(&t)
        #expect(t.update(state: incoming(), isMediaConnected: false, isDoNotDisturb: true) == [.end(id1, .unanswered)])
        #expect(!t.isManaging)
        #expect(!t.isAwaitingSlot)
        #expect(t.audioOwner == .app)
        #expect(t.update(state: incoming(), isMediaConnected: false).isEmpty)
        #expect(push(&t, state: incoming()).verdict == .dismissed)
    }

    @Test func aRefusedPushRemainsUnmanagedWhenItsProposalArrives() {
        var t = tracker()
        _ = push(&t)
        t.systemRefused(id2)
        #expect(t.isAwaitingSlot)
        t.systemRefused(id1)
        #expect(!t.isManaging)
        #expect(t.sid(for: id1) == nil)
        #expect(t.audioOwner == .app)
        #expect(t.update(state: .idle, isMediaConnected: false).isEmpty)
        #expect(push(&t).verdict == .dismissed)
        #expect(t.update(state: incoming(), isMediaConnected: false).isEmpty)
        #expect(t.audioOwner == .app)
    }

    @Test func aSystemEndedPushDoesNotReturnWhenItsProposalArrives() {
        var t = tracker()
        _ = push(&t)
        t.systemEnded(id2)
        #expect(t.isAwaitingSlot)
        t.systemEnded(id1)
        #expect(!t.isManaging)
        #expect(t.audioOwner == .nobody)
        #expect(t.update(state: .idle, isMediaConnected: false).isEmpty)
        #expect(push(&t).verdict == .dismissed)
        #expect(t.update(state: incoming(), isMediaConnected: false).isEmpty)
        #expect(t.audioOwner == .nobody)
    }

    @Test func anAdoptedCallIsAnsweredOnceWhenItLeavesIncoming() {
        var t = tracker()
        _ = push(&t)
        _ = t.update(state: incoming(), isMediaConnected: false)
        #expect(t.update(state: incoming(accepting: true), isMediaConnected: false).isEmpty)
        #expect(t.update(state: active(), isMediaConnected: false) == [.answer(id1)])
        #expect(t.update(state: active(), isMediaConnected: true).isEmpty)
    }

    @Test func anAnswerKnownWhileAwaitingIsNotEchoedAfterAdoption() {
        var t = tracker()
        _ = push(&t)
        t.systemAnswered(id1)
        #expect(t.update(state: .idle, isMediaConnected: false).isEmpty)
        #expect(t.update(state: incoming(accepting: true), isMediaConnected: false).isEmpty)
        #expect(t.update(state: active(), isMediaConnected: true).isEmpty)
    }

    @Test func anAnswerKnownWhileAwaitingCannotCarryOverToAnotherSid() {
        var t = tracker()
        _ = push(&t)
        t.systemAnswered(id1)
        #expect(t.update(state: incoming("s2", accepting: true), isMediaConnected: false) == [.end(id1, .unanswered)])
        #expect(!t.isManaging)
    }
}
