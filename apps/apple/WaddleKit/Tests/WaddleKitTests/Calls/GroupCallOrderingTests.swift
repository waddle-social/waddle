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

    @Test func rejoiningRightAfterTheMixerEndedAJoinIsNotCancelledByItsCleanup() async {
        var number = 0
        let f = GroupFixture(sid: {
            number += 1
            return "g\(number)"
        })
        let first = await f.begin()
        f.port.heldCallVerbs = ["reject"]
        f.store.handle(CallEvent(from: carol, sid: "c9", kind: .propose(callAudio)))
        await eventually { f.port.callSends.contains(.reject(carol, "c9", false)) }
        f.store.handle(CallEvent(from: groupMixer, sid: "g1", kind: .sessionTerminate(reason: .success)))
        let second = Task { await f.store.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        await eventually { f.store.state.sid == "g2" }
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        try? await Task.sleep(for: .milliseconds(50))
        // The room echoes the second attempt's preparing presence.
        f.echo()
        await eventually { f.port.callSends.contains(.groupInitiate(groupRoom, callOwn, "g2", false)) }
        #expect(f.store.state.sid == "g2")
        #expect(await !first.value)
        f.store.clear()
        _ = await second.value
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

    @Test func aFailedHandRaiseIsUndoneBeforeTheNextPresenceGoesOut() async {
        let f = GroupFixture()
        await f.activate()
        f.port.callSends = []
        f.port.heldCallVerbs = ["groupPresence"]
        f.port.callResults["groupPresence"] = false
        let raising = Task { await f.store.setHandRaised(true) }
        await eventually { f.port.callSends.count == 1 }
        let muting = Task { await f.store.advertiseSelfMute(true) }
        f.port.callResults["groupPresence"] = true
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await !raising.value)
        #expect(await muting.value)
        // What the room was last told matches what this device shows.
        #expect(f.port.callSends.last == .groupPresence(
            groupRoom, "alice", .init(state: .active(video: false), isHandRaised: false, isMuted: true)))
        #expect(!f.store.isHandRaised)
        f.store.clear()
    }

    @Test func aFailedHandToggleFallsBackToWhatTheRoomWasLastTold() async {
        let f = GroupFixture()
        await f.activate()
        f.port.callSends = []
        f.port.heldCallVerbs = ["groupPresence"]
        let muting = Task { await f.store.advertiseSelfMute(true) }
        await eventually { f.port.callSends.count == 1 }
        // Two toggles queue behind the mute; by the time they send, the hand is down again.
        let raising = Task { await f.store.setHandRaised(true) }
        let lowering = Task { await f.store.setHandRaised(false) }
        await Task.yield()
        f.port.releaseCallSends()
        await eventually { f.port.callSends.count == 2 }
        f.port.callResults["groupPresence"] = false
        f.port.releaseCallSends()
        await eventually { f.port.callSends.count == 3 }
        f.port.releaseCallSends()
        #expect(await muting.value)
        #expect(await raising.value)
        #expect(await !lowering.value)
        // Every presence that reached the room said the hand was down.
        #expect(!f.store.isHandRaised)
        f.store.clear()
    }

    @Test func aLateSendFromTheLastCallCannotSetThisCallsHandFallback() async {
        var number = 0
        let f = GroupFixture(sid: {
            number += 1
            return "g\(number)"
        })
        await f.activate()
        f.port.callSends = []
        // The last call's raised hand is still on its way when the next call starts.
        f.port.heldCallVerbs = ["groupPresence"]
        let raisingInFirst = Task { await f.store.setHandRaised(true) }
        await eventually { f.port.callSends.count == 1 }
        let leaving = Task { await f.store.hangUp() }
        let joining = Task { await f.store.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        await eventually { f.store.state.sid == "g2" }
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        await eventually { f.port.callSends.contains(.groupPresence(groupRoom, "alice", .init(state: .preparing))) }
        f.echo()
        await eventually { f.port.callSends.contains(.groupInitiate(groupRoom, callOwn, "g2", false)) }
        f.accept(sid: "g2")
        #expect(await joining.value)
        _ = await raisingInFirst.value
        await leaving.value
        // This call never told the room about a raised hand.
        f.port.callResults["groupPresence"] = false
        #expect(await !f.store.setHandRaised(true))
        #expect(!f.store.isHandRaised)
        f.store.clear()
    }

    @Test func losingTheStreamWhileJoiningAbandonsTheJoin() async {
        let f = GroupFixture()
        let joining = await f.begin()
        // What the session coordinator does when the stream drops.
        f.store.groupPresence.clear()
        #expect(await !joining.value)
        #expect(f.store.state == .ended(sid: "g1", reason: .error))
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
