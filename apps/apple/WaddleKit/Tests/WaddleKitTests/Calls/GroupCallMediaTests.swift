import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Group call media") struct GroupCallMediaTests {
    @Test func joiningResetsChoicesAndConnectsAgreedAudioWithIce() async {
        let media = RecordingCallMedia()
        let f = GroupFixture(media: media)
        f.port.externalServices = [
            ExternalService(kind: .stun, host: "stun.test", port: 3478),
            ExternalService(kind: .turns, host: "relay.test", port: 443, transport: .tcp, username: "u", password: "p")]
        let joining = await f.begin()
        #expect(media.captureChoiceResets == 1)
        #expect(media.calls.isEmpty)
        #expect(f.port.externalServicesFetchCount == 0)
        f.echo()
        await eventually { f.port.callSends.last == .groupInitiate(groupRoom, callOwn, "g1", false) }
        // The mixer names video too, but the agreed capture remains audio.
        f.accept()
        #expect(await joining.value)
        await eventually { !media.calls.isEmpty }
        #expect(media.calls == [.connect(groupJoin, callAudio, [
            IceServer(url: "stun:stun.test:3478"),
            IceServer(url: "turns:relay.test:443?transport=tcp", username: "u", credential: "p")])])
        #expect(f.port.externalServicesFetchCount == 1)
        #expect(f.store.state == .inGroup(GroupCall(room: groupRoom, sid: "g1", media: callAudio, join: groupJoin, nick: "alice")))
        f.store.clear()
    }

    @Test func mediaDeathEndsGroupAndLeavesBeforeTerminating() async {
        let media = RecordingCallMedia()
        let f = GroupFixture(media: media)
        await f.activate()
        await eventually { media.calls == [.connect(groupJoin, callAudio)] }
        f.port.callSends = []
        media.onFailure?()
        await eventually { f.port.callSends.last == .groupTerminate(groupRoom, "g1") }
        #expect(f.store.state == .ended(sid: "g1", reason: .finished(.connectivityError)))
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")])
        await eventually { media.calls.last == .disconnect }
        #expect(media.calls == [.connect(groupJoin, callAudio), .disconnect])
        f.store.clear()
    }

    @Test(arguments: [false, true])
    func unusableMediaEndsWithGoneAndFullGroupLeave(invalidGrant: Bool) async {
        let media = RecordingCallMedia()
        media.connectThrows = !invalidGrant
        let f = GroupFixture(media: media)
        let joining = await f.toInitiate()
        let join = invalidGrant
            ? CallJoin(url: groupJoin.url, room: groupJoin.room, identity: groupJoin.identity, token: "not-a-jwt")
            : groupJoin
        f.accept(join: join)
        #expect(await joining.value)
        await eventually { f.port.callSends.last == .groupTerminate(groupRoom, "g1") }
        #expect(f.store.state == .ended(sid: "g1", reason: .finished(.gone)))
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false))),
            .groupInitiate(groupRoom, callOwn, "g1", false),
            .groupPresence(groupRoom, "alice", .init(state: .left)), .groupTerminate(groupRoom, "g1")])
        if invalidGrant {
            #expect(media.calls == [.disconnect])
            #expect(f.port.externalServicesFetchCount == 0)
        } else {
            #expect(media.calls == [.connect(groupJoin, callAudio), .disconnect])
            #expect(f.port.externalServicesFetchCount == 1)
        }
        f.store.clear()
    }

    @Test(arguments: [false, true])
    func teardownDuringIceFetchCannotConnectOldGroup(clear: Bool) async {
        let media = RecordingCallMedia()
        let f = GroupFixture(media: media)
        f.port.holdExternalServicesFetch = true
        await f.activate()
        await eventually { f.port.externalServicesFetchCount == 1 }
        #expect(media.calls.isEmpty)
        if clear { f.store.clear() } else { await f.store.hangUp() }
        await eventually { media.calls == [.disconnect] }
        #expect(f.store.state == .idle)
        f.port.callSends = []
        await f.activate(media: callVideo)
        f.port.holdExternalServicesFetch = false
        f.port.releaseExternalServicesFetches()
        await eventually { media.calls.last == .connect(groupJoin, callVideo) }
        #expect(media.calls == [.disconnect, .connect(groupJoin, callVideo)])
        #expect(f.port.externalServicesFetchCount == 2)
        #expect(f.store.state == .inGroup(GroupCall(room: groupRoom, sid: "g1", media: callVideo, join: groupJoin, nick: "alice")))
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: true))),
            .groupInitiate(groupRoom, callOwn, "g1", true)])
        f.store.clear()
    }

    @Test func oldConnectFailureAndCallbackCannotEndReplacementGroupAfterClear() async throws {
        let media = RecordingCallMedia()
        media.holdConnect = true
        media.connectThrows = true
        let f = GroupFixture(media: media)
        await f.activate()
        await eventually { media.calls.count == 1 }
        let oldFailure = try #require(media.onFailure)
        f.store.clear()
        await eventually { media.calls.count == 2 }
        media.connectThrows = false
        f.port.callSends = []
        await f.activate(media: callVideo)
        media.releaseConnect()
        await eventually { media.calls.count == 4 }
        oldFailure()
        f.store.handle(CallEvent(from: callPeer, sid: "barrier", kind: .propose(callAudio)))
        await eventually { f.port.callSends.last == .reject(callPeer, "barrier", false) }
        #expect(media.calls == [
            .connect(groupJoin, callAudio), .disconnect, .disconnect, .connect(groupJoin, callVideo)])
        #expect(f.store.state == .inGroup(GroupCall(room: groupRoom, sid: "g1", media: callVideo, join: groupJoin, nick: "alice")))
        #expect(f.store.lastError == nil)
        #expect(f.port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .preparing)),
            .groupPresence(groupRoom, "alice", .init(state: .active(video: true))),
            .groupInitiate(groupRoom, callOwn, "g1", true), .reject(callPeer, "barrier", false)])
        f.store.clear()
    }
}
