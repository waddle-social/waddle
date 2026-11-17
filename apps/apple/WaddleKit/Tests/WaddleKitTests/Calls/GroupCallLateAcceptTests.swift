import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Group late accepts") struct GroupCallLateAcceptTests {
    @Test func acceptFirstArrivingAfterTeardownClosesTheHalfOpenMixer() async {
        let f = GroupFixture()
        f.port.heldCallVerbs = ["groupInitiate"]
        let joining = await f.toInitiate()
        await f.store.hangUp()
        #expect(f.store.state == .idle)
        f.accept(from: callPeer)
        f.accept(join: CallJoin(url: groupJoin.url, room: "elsewhere@muc.waddle.test", identity: groupJoin.identity, token: groupJoin.token))
        f.accept()
        await eventually { f.port.callSends.count == 6 }
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await !joining.value)
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
            .groupInitiate(groupRoom, callOwn, "g1", false),
            .groupPresence(groupRoom, "alice", .init(state: .left)),
            .groupTerminate(groupRoom, "g1"), .groupTerminate(groupRoom, "g1")])
        #expect(f.store.state == .idle)
        f.store.clear()
    }
}

extension GroupCallLateAcceptTests {
    @Test func resolvedAcceptCleanupWaitsForHeldLeave() async {
        let f = GroupFixture()
        let joining = await f.toInitiate()
        f.port.heldCallVerbs = ["groupPresence"]
        let leave = Task {
            f.accept()
            await f.store.hangUp()
        }
        await eventually { f.port.callSends.contains(.groupPresence(groupRoom, "alice", .init(state: .left))) }
        // A public arrival barrier lets the abandoned setup resume while leave is held.
        f.store.handle(CallEvent(from: callPeer, sid: "barrier", kind: .propose(callAudio)))
        await eventually { f.port.callSends.last == .ringing(callPeer.bare, "barrier") }
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
            .groupInitiate(groupRoom, callOwn, "g1", false),
            .groupPresence(groupRoom, "alice", .init(state: .left)), .ringing(callPeer.bare, "barrier")])
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        await leave.value
        #expect(await !joining.value)
        #expect(f.port.callSends.suffix(2) == [.groupTerminate(groupRoom, "g1"), .groupTerminate(groupRoom, "g1")])
        #expect(f.store.state == .incoming(IncomingCall(from: callPeer, sid: "barrier", media: callAudio)))
        f.store.clear()
    }
}
