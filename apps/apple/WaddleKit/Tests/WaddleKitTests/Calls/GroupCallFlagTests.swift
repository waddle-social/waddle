import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Group call flags") struct GroupCallFlagTests {
    @Test func handRaiseRestampsBothFlagsAndMutePreservesHand() async {
        let f = GroupFixture()
        await f.activate()
        #expect(await f.store.advertiseSelfMute(true))
        #expect(await f.store.setHandRaised(true))
        #expect(f.store.isHandRaised)
        #expect(f.store.isSelfMuteAdvertised)
        #expect(f.port.callSends.last == .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isHandRaised: true, isMuted: true)))
        #expect(await f.store.advertiseSelfMute(false))
        #expect(f.port.callSends.last == .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isHandRaised: true)))
        f.store.clear()
    }
}

extension GroupCallFlagTests {
    @Test func racingHandTogglesKeepWireOrder() async {
        let f = GroupFixture()
        await f.activate()
        f.port.callSends = []
        f.port.heldCallVerbs = ["groupPresence"]
        let raise = Task { await f.store.setHandRaised(true) }
        await eventually { f.port.callSends.count == 1 }
        #expect(f.store.isHandRaised)
        let lower = Task { await f.store.setHandRaised(false) }
        await eventually { !f.store.isHandRaised }
        #expect(f.port.callSends.count == 1)
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await raise.value)
        #expect(await lower.value)
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isHandRaised: true)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false)))])
        f.store.clear()
    }
}

extension GroupCallFlagTests {
    @Test func interleavedMuteAndHandCarryNewestFlags() async {
        let f = GroupFixture()
        await f.activate()
        f.port.callSends = []
        f.port.heldCallVerbs = ["groupPresence"]
        let mute = Task { await f.store.advertiseSelfMute(true) }
        await eventually { f.port.callSends.count == 1 }
        let raise = Task { await f.store.setHandRaised(true) }
        await eventually { f.store.isHandRaised }
        #expect(f.port.callSends.count == 1)
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await mute.value)
        #expect(await raise.value)
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isMuted: true)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isHandRaised: true, isMuted: true))])
        f.store.clear()
    }
}

extension GroupCallFlagTests {
    @Test func parkedRestampsSnapshotFinalFlags() async {
        let f = GroupFixture()
        await f.activate()
        f.port.callSends = []
        f.port.heldCallVerbs = ["groupPresence"]
        let mute = Task { await f.store.advertiseSelfMute(true) }
        await eventually { f.port.callSends.count == 1 }
        let raise = Task { await f.store.setHandRaised(true) }
        await eventually { f.store.isHandRaised }
        let lower = Task { await f.store.setHandRaised(false) }
        await eventually { !f.store.isHandRaised }
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await mute.value)
        #expect(await raise.value)
        #expect(await lower.value)
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isMuted: true)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isMuted: true)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isMuted: true))])
        f.store.clear()
    }
}

extension GroupCallFlagTests {
    @Test func parkedRestampStandsDownAfterHangUp() async {
        let f = GroupFixture()
        await f.activate()
        f.port.callSends = []
        f.port.heldCallVerbs = ["groupPresence"]
        let raise = Task { await f.store.setHandRaised(true) }
        await eventually { f.port.callSends.count == 1 }
        let mute = Task { await f.store.advertiseSelfMute(true) }
        await eventually { f.store.isSelfMuteAdvertised }
        let leave = Task { await f.store.hangUp() }
        await eventually { f.store.state == .idle }
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await raise.value)
        #expect(await !mute.value)
        await leave.value
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isHandRaised: true)),
            .groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")])
        #expect(!f.store.isHandRaised)
        #expect(!f.store.isSelfMuteAdvertised)
        #expect(f.store.lastError == nil)
        f.store.clear()
    }
}

extension GroupCallFlagTests {
    @Test func failedHandRollsBackButMuteDoesNot() async {
        let f = GroupFixture()
        await f.activate()
        f.port.callResults["groupPresence"] = false
        #expect(await !f.store.setHandRaised(true))
        #expect(!f.store.isHandRaised)
        #expect(f.store.lastError == "muji hand-raise update failed")
        #expect(await !f.store.advertiseSelfMute(true))
        #expect(f.store.isSelfMuteAdvertised)
        #expect(f.store.lastError == "muji mute update failed")
        f.store.clear()
    }
}

extension GroupCallFlagTests {
    @Test func theRoomIsOnlyKnownToHaveHeardAMuteThatWasSent() async {
        let f = GroupFixture()
        await f.activate()
        #expect(!f.store.confirmedSelfMute)
        f.port.callResults["groupPresence"] = false
        #expect(await !f.store.advertiseSelfMute(true))
        // What we meant to say and what the room heard now differ, so
        // whoever keeps them in step knows there is still something to send.
        #expect(f.store.isSelfMuteAdvertised)
        #expect(!f.store.confirmedSelfMute)
        f.port.callResults["groupPresence"] = true
        // Any presence that gets through carries the mute with it.
        #expect(await f.store.setHandRaised(true))
        #expect(f.store.confirmedSelfMute)
        f.store.clear()
        #expect(!f.store.confirmedSelfMute)
    }

    @Test func aMuteThatFailedIsStillOwedAfterTheMicrophoneComesBackOn() async {
        let f = GroupFixture()
        await f.activate()
        f.port.callResults["groupPresence"] = false
        #expect(await !f.store.advertiseSelfMute(true))
        // The room still thinks we are unmuted, which is true again, but
        // the next presence would say muted: that is owed a correction.
        #expect(f.store.isSelfMuteOwed(false))
        f.port.callResults["groupPresence"] = true
        #expect(await f.store.advertiseSelfMute(false))
        #expect(!f.store.isSelfMuteOwed(false))
        f.port.callSends.removeAll()
        #expect(await f.store.setHandRaised(true))
        #expect(f.port.callSends == [.groupPresence(groupRoom, "alice", .init(state: .active(video: false), isHandRaised: true, isMuted: false))])
        f.store.clear()
    }

    @Test func flagRestampsRequireActiveGroup() async {
        let f = GroupFixture()
        #expect(await !f.store.setHandRaised(true))
        #expect(await !f.store.advertiseSelfMute(true))
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(CallEvent(from: callPeer, sid: "g1", kind: .sessionAccept(join: callJoin, media: callAudio)))
        #expect(await !f.store.setHandRaised(true))
        #expect(await !f.store.advertiseSelfMute(true))
        #expect(f.port.callSends == [.propose(callPeer.bare, "g1", callAudio)])
        f.store.clear()
    }
}

extension GroupCallFlagTests {
    @Test func failedOldRaiseCannotUndoANewerRaiseAfterLowering() async {
        let f = GroupFixture()
        await f.activate()
        f.port.callSends = []
        f.port.heldCallVerbs = ["groupPresence"]
        f.port.callResults["groupPresence"] = false
        let firstRaise = Task { await f.store.setHandRaised(true) }
        await eventually { f.port.callSends.count == 1 }
        let lower = Task { await f.store.setHandRaised(false) }
        await eventually { !f.store.isHandRaised }
        let newerRaise = Task { await f.store.setHandRaised(true) }
        await eventually { f.store.isHandRaised }
        f.port.callResults["groupPresence"] = true
        f.port.heldCallVerbs = []
        f.port.releaseCallSends()
        #expect(await !firstRaise.value)
        #expect(await lower.value)
        #expect(await newerRaise.value)
        #expect(f.store.isHandRaised)
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isHandRaised: true)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isHandRaised: true)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isHandRaised: true))])
        f.store.clear()
    }
}
