import Foundation
import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Pushed session calls") struct PushedSessionCallTests {
    private func session() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(
            account: AccountIdentity(jid: callOwn.bare, nick: "alice", resource: callOwn.resource),
            port: port, handlesCalls: true)
        return (coordinator, port)
    }

    private func push() -> CallPush {
        CallPush(userInfo: ["waddle": [
            "v": 1, "class": "call", "conversation": callPeer.bare.description,
            "item": "stanza", "node": "voip-node",
            "call": ["sid": "c-push", "video": true, "expires": 4_102_444_800],
        ]])!
    }

    private func report(_ push: CallPush, to tracker: inout SystemCallTracker, calls: CallStore) -> PushedCall {
        tracker.pushed(
            sid: push.sid, conversation: push.conversation, isVideo: push.isVideo,
            isExpired: push.isExpired(at: .now), isFinished: calls.isFinished(sid: push.sid), state: calls.state)
    }

    private func drain(_ store: CallStore) async {
        var drained = false
        store.enqueue { drained = true }
        await eventually { drained }
        #expect(drained)
    }

    @Test func aPushReportsWithoutChangingTheSlot() async {
        let (c, port) = session()
        var tracker = SystemCallTracker()
        let result = report(push(), to: &tracker, calls: c.calls)
        await drain(c.calls)

        #expect(result.verdict == .awaiting)
        #expect(result.updates.count == 1)
        #expect(tracker.isAwaitingSlot)
        #expect(tracker.managedCall?.sid == "c-push")
        #expect(c.calls.state == .idle)
        #expect(port.callSends.isEmpty)
        await c.stop()
    }

    @Test func aStreamProposalAfterThePushRingsOnce() async {
        let (c, port) = session()
        var tracker = SystemCallTracker()
        #expect(report(push(), to: &tracker, calls: c.calls).verdict == .awaiting)
        let systemCall = tracker.managedCall
        let proposal = CallEvent(from: callPeer, sid: "c-push", kind: .propose(callVideo))

        c.handle(.call(proposal))
        #expect(tracker.update(state: c.calls.state, isMediaConnected: false).isEmpty)
        c.handle(.call(proposal))
        #expect(report(push(), to: &tracker, calls: c.calls).verdict == .duplicate)
        await drain(c.calls)

        #expect(c.calls.state == .incoming(IncomingCall(from: callPeer, sid: "c-push", media: callVideo)))
        #expect(!tracker.isAwaitingSlot)
        #expect(tracker.managedCall == systemCall)
        #expect(port.callSends == [.ringing(callPeer, "c-push")])
        await c.stop()
    }

    @Test func aStreamProposalThenRetractLeavesNothingRinging() async throws {
        let (c, port) = session()
        var tracker = SystemCallTracker()
        #expect(report(push(), to: &tracker, calls: c.calls).verdict == .awaiting)
        let id = try #require(tracker.managedCall?.id)

        c.handle(.call(CallEvent(from: callPeer, sid: "c-push", kind: .propose(callVideo))))
        #expect(tracker.update(state: c.calls.state, isMediaConnected: false).isEmpty)
        c.handle(.call(CallEvent(from: callPeer, sid: "c-push", kind: .retract(reason: nil, tieBreak: false))))
        #expect(tracker.update(state: c.calls.state, isMediaConnected: false) == [.end(id, .remoteEnded)])
        await drain(c.calls)

        #expect(c.calls.state == .ended(sid: "c-push", reason: .retracted))
        #expect(!tracker.isManaging)
        #expect(!tracker.isAwaitingSlot)
        #expect(port.callSends.isEmpty)
        await c.stop()
    }

    @Test func aFinishedSidNeverRingsFromItsPushOrStreamProposal() async {
        let (c, port) = session()
        c.calls.markFinished(sid: "c-push")
        var tracker = SystemCallTracker()
        #expect(report(push(), to: &tracker, calls: c.calls).verdict == .dismissed)

        c.handle(.call(CallEvent(from: callPeer, sid: "c-push", kind: .propose(callVideo))))
        await drain(c.calls)

        #expect(c.calls.state == .idle)
        #expect(!tracker.isManaging)
        #expect(port.callSends.isEmpty)
        await c.stop()
    }
}
