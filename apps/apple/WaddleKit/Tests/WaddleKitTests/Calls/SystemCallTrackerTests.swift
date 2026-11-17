import Foundation
import Testing
import WaddleKit

@Suite("System call tracker") struct SystemCallTrackerTests {
    private let peer = JID(parsing: "bob@waddle.test/phone")!
    private let own = JID(parsing: "me@waddle.test/mac")!
    private let room = BareJID(parsing: "room@muc.waddle.test")!
    private let audio = CallMedia(audio: true, video: false)
    private let join = CallJoin(url: "wss://sfu.waddle.test", room: "r", identity: "me@waddle.test/mac", token: "t")
    private let id1 = UUID(uuidString: "00000000-0000-0000-0000-000000000001")!
    private let id2 = UUID(uuidString: "00000000-0000-0000-0000-000000000002")!

    private func tracker() -> SystemCallTracker {
        var next = [id2, id1]
        return SystemCallTracker(newID: { next.popLast() ?? UUID() })
    }

    private func incoming(_ sid: String = "s1", accepting: Bool = false, video: Bool = false) -> CallState {
        .incoming(IncomingCall(from: peer, sid: sid, media: CallMedia(audio: true, video: video), isAccepting: accepting))
    }

    private func outgoing(_ sid: String = "s1", responder: JID? = nil) -> CallState {
        .outgoing(OutgoingCall(to: peer.bare, sid: sid, media: audio, initiator: own, isRinging: true, responder: responder))
    }

    private func active(_ sid: String = "s1") -> CallState {
        .active(ActiveCall(peer: peer, sid: sid, media: audio, join: join))
    }

    // MARK: - Incoming

    @Test func anIncomingCallIsReportedOnce() {
        var t = tracker()
        let call = SystemCall(id: id1, sid: "s1", direction: .incoming, conversation: .direct(peer.bare), isVideo: true)
        #expect(t.update(state: incoming(video: true), isMediaConnected: false) == [.reportIncoming(call)])
        #expect(t.update(state: incoming(video: true), isMediaConnected: false).isEmpty)
        #expect(t.isManaging)
    }

    @Test func answeringInTheAppTellsTheSystemOnceTheCallIsReallyAnswered() {
        var t = tracker()
        _ = t.update(state: incoming(), isMediaConnected: false)
        // The answer is still on its way and may yet fail.
        #expect(t.update(state: incoming(accepting: true), isMediaConnected: false).isEmpty)
        #expect(t.update(state: active(), isMediaConnected: false) == [.answer(id1)])
        #expect(t.update(state: active(), isMediaConnected: true).isEmpty)
    }

    @Test func answeringOnTheSystemScreenIsNotEchoedBack() {
        var t = tracker()
        _ = t.update(state: incoming(), isMediaConnected: false)
        _ = t.update(state: incoming(accepting: true), isMediaConnected: false)
        t.systemAnswered(id1)
        #expect(t.update(state: active(), isMediaConnected: true).isEmpty)
    }

    @Test func anAcceptThatFailedLeavesTheSystemStillRinging() {
        var t = tracker()
        _ = t.update(state: incoming(), isMediaConnected: false)
        _ = t.update(state: incoming(accepting: true), isMediaConnected: false)
        #expect(t.update(state: incoming(), isMediaConnected: false).isEmpty)
        #expect(t.update(state: incoming(accepting: true), isMediaConnected: false).isEmpty)
        #expect(t.update(state: active(), isMediaConnected: false) == [.answer(id1)])
    }

    // MARK: - Who owns audio

    @Test func theSystemOwnsAudioForACallItManages() {
        var t = tracker()
        #expect(t.audioOwner == .nobody)
        _ = t.update(state: incoming(), isMediaConnected: false)
        #expect(t.audioOwner == .system)
        _ = t.update(state: active(), isMediaConnected: true)
        #expect(t.audioOwner == .system)
        _ = t.update(state: .ended(sid: "s1", reason: .finished(.success)), isMediaConnected: false)
        #expect(t.audioOwner == .nobody)
    }

    @Test func theAppOwnsAudioForACallLeftToIt() {
        var t = tracker()
        _ = t.update(state: incoming(), isMediaConnected: false, isDoNotDisturb: true)
        #expect(t.audioOwner == .app)
        _ = t.update(state: .idle, isMediaConnected: false)
        #expect(t.audioOwner == .nobody)
        _ = t.update(state: incoming("s2"), isMediaConnected: false)
        t.systemRefused(id1)
        #expect(t.audioOwner == .app)
    }

    @Test func nobodyOwnsAudioOnceTheSystemEndedTheCall() {
        var t = tracker()
        _ = t.update(state: active(), isMediaConnected: false)
        _ = t.update(state: .idle, isMediaConnected: false)
        _ = t.update(state: outgoing("s2"), isMediaConnected: false)
        t.systemEnded(id1)
        // The slot still holds the call for a moment while it is torn
        // down; its media must not start again.
        #expect(t.update(state: outgoing("s2"), isMediaConnected: false).isEmpty)
        #expect(t.audioOwner == .nobody)
        _ = t.update(state: .idle, isMediaConnected: false)
        _ = t.update(state: incoming("s3"), isMediaConnected: false)
        #expect(t.audioOwner == .system)
    }

    @Test(arguments: [
        (CallEndReason.retracted, SystemCallEnd.remoteEnded), (.missed, .unanswered), (.expired, .answeredElsewhere),
        (.error, .failed), (.finished(.success), .remoteEnded), (.finished(.connectivityError), .failed),
    ])
    func anIncomingCallEndsWithWhatHappened(reason: CallEndReason, expected: SystemCallEnd) {
        var t = tracker()
        _ = t.update(state: incoming(), isMediaConnected: false)
        #expect(t.update(state: .ended(sid: "s1", reason: reason), isMediaConnected: false) == [.end(id1, expected)])
        #expect(!t.isManaging)
        #expect(t.update(state: .idle, isMediaConnected: false).isEmpty)
    }

    @Test func aCallEndedInTheAppIsReportedEnded() {
        var t = tracker()
        _ = t.update(state: incoming(), isMediaConnected: false)
        #expect(t.update(state: .idle, isMediaConnected: false) == [.end(id1, .remoteEnded)])
    }

    @Test func aCallEndedOnTheSystemScreenIsNotReportedBack() {
        var t = tracker()
        _ = t.update(state: incoming(), isMediaConnected: false)
        t.systemEnded(id1)
        #expect(t.update(state: .idle, isMediaConnected: false).isEmpty)
        #expect(!t.isManaging)
    }

    // MARK: - What the system is not told

    @Test func doNotDisturbKeepsTheCallOffTheSystemScreenForGood() {
        var t = tracker()
        #expect(t.update(state: incoming(), isMediaConnected: false, isDoNotDisturb: true).isEmpty)
        // Turning it off mid-ring does not start a ring late.
        #expect(t.update(state: incoming(), isMediaConnected: false).isEmpty)
        #expect(t.update(state: incoming(accepting: true), isMediaConnected: false).isEmpty)
        #expect(t.update(state: active(), isMediaConnected: true).isEmpty)
        #expect(!t.isManaging)
        // The next call is a new call.
        _ = t.update(state: .idle, isMediaConnected: false)
        #expect(t.update(state: incoming("s2"), isMediaConnected: false).count == 1)
    }

    @Test func aCallTheSystemRefusedIsLeftToTheApp() {
        var t = tracker()
        _ = t.update(state: incoming(), isMediaConnected: false)
        t.systemRefused(id1)
        #expect(!t.isManaging)
        #expect(t.update(state: incoming(accepting: true), isMediaConnected: false).isEmpty)
        #expect(t.update(state: .idle, isMediaConnected: false).isEmpty)
    }

    @Test func aCallAlreadyAnsweredWhenFirstSeenIsLeftToTheApp() {
        var t = tracker()
        #expect(t.update(state: active(), isMediaConnected: false).isEmpty)
        #expect(!t.isManaging)
    }

    // MARK: - Outgoing

    @Test func aPlacedCallIsStartedThenConnectingThenConnected() {
        var t = tracker()
        let call = SystemCall(id: id1, sid: "s1", direction: .outgoing, conversation: .direct(peer.bare), isVideo: false)
        #expect(t.update(state: outgoing(), isMediaConnected: false) == [.startOutgoing(call)])
        #expect(t.update(state: outgoing(responder: peer), isMediaConnected: false) == [.connecting(id1)])
        #expect(t.update(state: active(), isMediaConnected: false).isEmpty)
        #expect(t.update(state: active(), isMediaConnected: true) == [.connected(id1)])
        #expect(t.update(state: active(), isMediaConnected: true).isEmpty)
    }

    @Test func aPlacedCallThatSkipsAStepStillPassesThroughIt() {
        var t = tracker()
        _ = t.update(state: outgoing(), isMediaConnected: false)
        #expect(t.update(state: active(), isMediaConnected: true) == [.connecting(id1), .connected(id1)])
    }

    @Test func aDeclinedPlacedCallEnds() {
        var t = tracker()
        _ = t.update(state: outgoing(), isMediaConnected: false)
        #expect(t.update(state: .ended(sid: "s1", reason: .rejected), isMediaConnected: false) == [.end(id1, .remoteEnded)])
        _ = t.update(state: outgoing("s2"), isMediaConnected: false)
        #expect(t.update(state: .ended(sid: "s2", reason: .timeout), isMediaConnected: false) == [.end(id2, .unanswered)])
    }

    @Test func aRoomsCallIsAPlacedCallToTheRoom() {
        var t = tracker()
        let attempt = GroupCallAttempt(room: room, sid: "g1", media: CallMedia(audio: true, video: true), nick: "me")
        let call = SystemCall(id: id1, sid: "g1", direction: .outgoing, conversation: .room(room), isVideo: true)
        #expect(t.update(state: .joiningGroup(attempt), isMediaConnected: false) == [.startOutgoing(call)])
        let joined = CallState.inGroup(GroupCall(room: room, sid: "g1", media: attempt.media, join: join, nick: "me"))
        #expect(t.update(state: joined, isMediaConnected: false) == [.connecting(id1)])
        #expect(t.update(state: joined, isMediaConnected: true) == [.connected(id1)])
        #expect(t.update(state: .idle, isMediaConnected: false) == [.end(id1, .remoteEnded)])
    }

    // MARK: - One call after another

    @Test func aNewCallInTheSlotEndsTheOldOneFirst() {
        var t = tracker()
        _ = t.update(state: outgoing("s1"), isMediaConnected: false)
        let next = SystemCall(id: id2, sid: "s2", direction: .incoming, conversation: .direct(peer.bare), isVideo: false)
        // We lost a tie-break: ours ends and theirs rings.
        #expect(t.update(state: incoming("s2"), isMediaConnected: false) == [.end(id1, .remoteEnded), .reportIncoming(next)])
    }

    @Test func theCallForAnIdentifierIsFoundWhileItLasts() {
        var t = tracker()
        _ = t.update(state: incoming(), isMediaConnected: false)
        #expect(t.sid(for: id1) == "s1")
        #expect(t.sid(for: id2) == nil)
        _ = t.update(state: .idle, isMediaConnected: false)
        #expect(t.sid(for: id1) == nil)
    }
}
