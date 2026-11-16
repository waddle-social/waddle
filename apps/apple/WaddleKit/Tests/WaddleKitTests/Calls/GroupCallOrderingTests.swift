import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Group call ordering") struct GroupCallOrderingTests {
    private let carol = JID(parsing: "carol@waddle.test/tab")!

    @Test func theMixerEndingACallIsNotHeldBehindAnUnrelatedSend() async {
        let f = GroupFixture()
        await f.activate()
        // A busy-reject for someone else is stuck on the wire.
        f.port.heldCallVerbs = ["reject"]
        f.store.handle(CallEvent(from: carol, sid: "c9", kind: .propose(callAudio)))
        await eventually { f.port.callSends.contains(.reject(carol, "c9", false)) }
        f.store.handle(CallEvent(from: groupMixer, sid: "g1", kind: .sessionTerminate(reason: .success)))
        #expect(f.store.state == .ended(sid: "g1", reason: .finished(.success)))
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        await eventually { f.port.callSends.contains(.groupTerminate(groupRoom, "g1")) }
        f.store.clear()
    }

    @Test func anAcceptArrivingAfterTheMixerEndedTheCallDoesNotJoinIt() async {
        let f = GroupFixture()
        let joining = await f.toInitiate()
        f.port.heldCallVerbs = ["reject"]
        f.store.handle(CallEvent(from: carol, sid: "c9", kind: .propose(callAudio)))
        await eventually { f.port.callSends.contains(.reject(carol, "c9", false)) }
        f.store.handle(CallEvent(from: groupMixer, sid: "g1", kind: .sessionTerminate(reason: .success)))
        f.accept()
        #expect(await !joining.value)
        #expect(f.store.state == .ended(sid: "g1", reason: .finished(.success)))
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        f.store.clear()
    }

    @Test func rejoiningARoomWaitsForTheLastCallsLeaveAndTerminate() async {
        var number = 0
        let f = GroupFixture(sid: {
            number += 1
            return "g\(number)"
        })
        await f.activate()
        f.port.callSends = []
        f.port.heldCallVerbs = ["groupTerminate"]
        let leaving = Task { await f.store.hangUp() }
        await eventually { f.port.callSends.contains(.groupTerminate(groupRoom, "g1")) }
        let rejoining = Task { await f.store.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        try? await Task.sleep(for: .milliseconds(100))
        // The old session's terminate would clear a newer advertisement at the mixer.
        #expect(f.port.callSends == [.groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")])
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        await eventually { f.port.callSends.contains(.groupPresence(groupRoom, "alice", .init(state: .preparing))) }
        await leaving.value
        f.store.clear()
        _ = await rejoining.value
    }

    @Test func losingTheStreamWhileJoiningAbandonsTheJoin() async {
        let f = GroupFixture()
        let joining = await f.begin()
        // What the session coordinator does when the stream drops.
        f.store.groupPresence.clear()
        #expect(await !joining.value)
        #expect(f.store.state == .idle)
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .left)),
        ])
        f.store.clear()
    }

    @Test func anOccupantWhosePresenceErrorsLeavesTheCallRoster() {
        let presence = GroupCallPresence()
        presence.apply(groupWire(nick: "bob", media: callAudio, realJID: callPeer))
        presence.apply(WirePresence(from: groupRoom.with(resource: "bob")!, kind: .error(condition: "not-authorized")))
        #expect(presence.participants[groupRoom] == nil)
    }
}
