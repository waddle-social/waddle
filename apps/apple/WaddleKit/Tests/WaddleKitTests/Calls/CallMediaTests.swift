import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite("Call media") struct CallMediaTests {
    @Test(arguments: [false, true])
    func teardownDuringExternalServicesFetchNeverConnectsTheOldCall(clear: Bool) async {
        let port = FakePort()
        port.holdExternalServicesFetch = true
        let media = RecordingCallMedia()
        let store = CallStore(port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare, media: media, newSid: { "c-reused" })
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        store.handle(CallEvent(from: callPeer, sid: "c-reused", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { port.externalServicesFetchCount == 1 }
        #expect(port.externalServicesFetchCount == 1)
        #expect(media.calls.isEmpty)
        if clear { store.clear() } else { await store.hangUp() }
        await eventually { media.calls == [.disconnect] }
        #expect(store.state == .idle)
        #expect(await store.startCall(to: callPeer.bare, media: callVideo))
        store.handle(CallEvent(from: callPeer, sid: "c-reused", kind: .sessionAccept(join: callJoin, media: callVideo)))
        port.holdExternalServicesFetch = false
        port.releaseExternalServicesFetches()
        // Replacement connect waits for the old task: it is a completion
        // barrier for any stale fetch continuation or late cleanup.
        await eventually { media.calls.last == .connect(callJoin, callVideo) }
        #expect(media.calls == [.disconnect, .connect(callJoin, callVideo)])
        #expect(port.externalServicesFetchCount == 2)
        #expect(store.state == .active(ActiveCall(peer: callPeer, sid: "c-reused", media: callVideo, join: callJoin, initiator: callOwn)))
        let teardown: [CallSend] = clear ? [] : [
            .terminate(callPeer, "c-reused", .success), .finish(callPeer, "c-reused", nil),
        ]
        #expect(port.callSends == [.propose(callPeer.bare, "c-reused", callAudio)] + teardown + [
            .propose(callPeer.bare, "c-reused", callVideo)
        ])
        store.clear()
    }

    @Test func invalidGrantDisconnectsWithoutFetchingOrConnectingAndEndsWithGone() async {
        let port = FakePort()
        let media = RecordingCallMedia()
        let store = CallStore(port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare, media: media, newSid: { "c-invalid" })
        let invalid = CallJoin(url: callJoin.url, room: callJoin.room, identity: callJoin.identity, token: "not-a-jwt")
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        store.handle(CallEvent(from: callPeer, sid: "c-invalid", kind: .sessionAccept(join: invalid, media: callAudio)))
        await eventually { port.callSends.count == 3 }
        #expect(store.state == .ended(sid: "c-invalid", reason: .finished(.gone)))
        #expect(media.calls == [.disconnect])
        #expect(port.externalServicesFetchCount == 0)
        #expect(port.callSends == [
            .propose(callPeer.bare, "c-invalid", callAudio),
            .terminate(callPeer, "c-invalid", .gone), .finish(callPeer, "c-invalid", nil),
        ])
        store.clear()
    }

    @Test func externalServicesReachMediaAsIceServers() async {
        let port = FakePort()
        port.externalServices = [
            ExternalService(kind: .stun, host: "stun.test", port: 3478),
            ExternalService(kind: .turns, host: "2001:db8::1", port: 443, transport: .tcp, username: "u", password: "p"),
            ExternalService(kind: .turn, host: "unusable.test"),
        ]
        let media = RecordingCallMedia()
        let store = CallStore(port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare, media: media, newSid: { "c-ice" })
        #expect(await store.startCall(to: callPeer.bare, media: callVideo))
        store.handle(CallEvent(from: callPeer, sid: "c-ice", kind: .sessionAccept(join: callJoin, media: callVideo)))
        await eventually { !media.calls.isEmpty }
        #expect(media.calls == [.connect(callJoin, callVideo, [
            IceServer(url: "stun:stun.test:3478"),
            IceServer(url: "turns:[2001:db8::1]:443?transport=tcp", username: "u", credential: "p"),
        ])])
        #expect(port.externalServicesFetchCount == 1)
        store.clear()
    }

    @Test func oldConnectFailureAndCallbackCannotEndReusedSIDAfterClear() async throws {
        let port = FakePort()
        let media = RecordingCallMedia()
        media.holdConnect = true
        media.connectThrows = true
        let store = CallStore(
            port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare,
            media: media, newSid: { "c-reused" })
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        store.handle(CallEvent(from: callPeer, sid: "c-reused", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        let oldFailure = try #require(media.onFailure)
        store.clear()
        await eventually { media.calls.count == 2 }
        media.connectThrows = false
        #expect(await store.startCall(to: callPeer.bare, media: callVideo))
        store.handle(CallEvent(from: callPeer, sid: "c-reused", kind: .sessionAccept(join: callJoin, media: callVideo)))
        media.releaseConnect()
        await eventually { media.calls.count == 4 }
        oldFailure()
        let carol = JID(parsing: "carol@waddle.test/tab")!
        store.handle(CallEvent(from: carol, sid: "c-barrier", kind: .propose(callAudio)))
        await eventually { port.callSends.last == .reject(carol, "c-barrier", false) }
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect, .disconnect, .connect(callJoin, callVideo)])
        #expect(store.state == .active(ActiveCall(peer: callPeer, sid: "c-reused", media: callVideo, join: callJoin, initiator: callOwn)))
        #expect(store.lastError == nil)
        #expect(
            port.callSends == [
                .propose(callPeer.bare, "c-reused", callAudio), .propose(callPeer.bare, "c-reused", callVideo),
                .reject(carol, "c-barrier", false),
            ])
        store.clear()
    }

    @Test func migrationDisconnectsInIncomingPhaseThenConnectsNewSession() async {
        let port = FakePort()
        let media = RecordingCallMedia()
        let store = CallStore(
            port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare,
            media: media, newSid: { "c-old" })
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        store.handle(CallEvent(from: callPeer, sid: "c-old", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        store.handle(CallEvent(from: callPeer, sid: "c-new", kind: .propose(callVideo)))
        await eventually { media.calls.count == 2 }
        // The peer asks for video, but the user only ever agreed to audio.
        #expect(store.state == .incoming(IncomingCall(from: callPeer, sid: "c-new", media: callAudio, isAccepting: true)))
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect])
        let migratedJoin = CallJoin(
            url: callJoin.url, room: "migrated-room", identity: callJoin.identity,
            token: callToken(#"{"sub":"alice@waddle.test/waddle-apple-1","video":{"roomJoin":true,"room":"migrated-room"}}"#))
        store.handle(CallEvent(from: callPeer, sid: "c-new", kind: .sessionInitiate(join: migratedJoin, media: callVideo)))
        await eventually { media.calls.count == 3 }
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect, .connect(migratedJoin, callAudio)])
        #expect(store.state == .active(ActiveCall(peer: callPeer, sid: "c-new", media: callAudio, join: migratedJoin, initiator: callPeer)))
        store.clear()
    }

    @Test func replacementMediaWaitsForCancelledConnectCleanupEvenWhenSIDIsReused() async {
        let port = FakePort()
        let media = RecordingCallMedia()
        media.holdConnect = true
        let store = CallStore(
            port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare,
            media: media, newSid: { "c-reused" })
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        store.handle(CallEvent(from: callPeer, sid: "c-reused", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        store.clear()
        await eventually { media.calls.count == 2 }
        #expect(await store.startCall(to: callPeer.bare, media: callVideo))
        store.handle(CallEvent(from: callPeer, sid: "c-reused", kind: .sessionAccept(join: callJoin, media: callVideo)))
        media.releaseConnect()
        await eventually { media.calls.count == 4 }
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect, .disconnect, .connect(callJoin, callVideo)])
        #expect(store.state == .active(ActiveCall(peer: callPeer, sid: "c-reused", media: callVideo, join: callJoin, initiator: callOwn)))
        #expect(port.callSends == [.propose(callPeer.bare, "c-reused", callAudio), .propose(callPeer.bare, "c-reused", callVideo)])
        store.clear()
    }

    @Test func hangUpDisconnectsWhileConnectIsHeldAndCleansItsLateCompletion() async {
        let port = FakePort()
        let media = RecordingCallMedia()
        media.holdConnect = true
        let store = CallStore(
            port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare,
            media: media, newSid: { "c-slow" })
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        store.handle(CallEvent(from: callPeer, sid: "c-slow", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        await store.hangUp()
        await eventually { media.calls.count == 2 }
        #expect(store.state == .idle)
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect])
        #expect(
            port.callSends == [
                .propose(callPeer.bare, "c-slow", callAudio), .terminate(callPeer, "c-slow", .success), .finish(callPeer, "c-slow", nil),
            ])
        media.releaseConnect()
        await eventually { media.calls.count == 3 }
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect, .disconnect])
        #expect(store.state == .idle)
        #expect(
            port.callSends == [
                .propose(callPeer.bare, "c-slow", callAudio), .terminate(callPeer, "c-slow", .success), .finish(callPeer, "c-slow", nil),
            ])
        store.clear()
    }

    @Test func establishedMediaDeathTerminatesWithConnectivityError() async {
        let port = FakePort()
        let media = RecordingCallMedia()
        let store = CallStore(
            port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare,
            media: media, newSid: { "c-media" })
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        store.handle(CallEvent(from: callPeer, sid: "c-media", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        media.onFailure?()
        await eventually { port.callSends.count == 3 && media.calls.count == 2 }
        #expect(store.state == .ended(sid: "c-media", reason: .finished(.connectivityError)))
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect])
        #expect(
            port.callSends == [
                .propose(callPeer.bare, "c-media", callAudio), .terminate(callPeer, "c-media", .connectivityError),
                .finish(callPeer, "c-media", nil),
            ])
        store.clear()
    }

    @Test func impossibleMediaDisconnectsAndTerminatesWithGone() async {
        let port = FakePort()
        let media = RecordingCallMedia()
        media.connectThrows = true
        let store = CallStore(
            port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare,
            media: media, newSid: { "c-media" })
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        store.handle(CallEvent(from: callPeer, sid: "c-media", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { port.callSends.count == 3 }
        #expect(store.state == .ended(sid: "c-media", reason: .finished(.gone)))
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect])
        #expect(
            port.callSends == [
                .propose(callPeer.bare, "c-media", callAudio), .terminate(callPeer, "c-media", .gone), .finish(callPeer, "c-media", nil),
            ])
        store.clear()
    }

    @Test func remoteEndDisconnectsMedia() async {
        let port = FakePort()
        let media = RecordingCallMedia()
        let store = CallStore(
            port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare,
            media: media, newSid: { "c-media" })
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        store.handle(CallEvent(from: callPeer, sid: "c-media", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        store.handle(CallEvent(from: callPeer, sid: "c-media", kind: .sessionTerminate(reason: .success)))
        await eventually { media.calls.count == 2 }
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect])
        #expect(store.state == .ended(sid: "c-media", reason: .finished(.success)))
        store.clear()
    }

    @Test func activeSessionConnectsWithServerCredentialsAndRequestedTracks() async {
        let port = FakePort()
        let media = RecordingCallMedia()
        let store = CallStore(
            port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare,
            media: media, newSid: { "c-media" })
        #expect(await store.startCall(to: callPeer.bare, media: callVideo))
        store.handle(CallEvent(from: callPeer, sid: "c-media", kind: .sessionAccept(join: callJoin, media: callVideo)))
        await eventually { media.calls == [.connect(callJoin, callVideo)] }
        #expect(media.calls == [.connect(callJoin, callVideo)])
        store.clear()
    }
}

@MainActor final class RecordingCallMedia: CallMediaController {
    enum Call: Equatable {
        case connect(CallJoin, CallMedia, [IceServer] = [])
        case disconnect
    }
    var onFailure: (@MainActor () -> Void)?
    var calls: [Call] = []
    var connectThrows = false
    var holdConnect = false
    private var heldConnect: CheckedContinuation<Void, Never>?
    private enum Failure: Error { case unavailable }

    func connect(join: CallJoin, media: CallMedia, iceServers: [IceServer]) async throws {
        let shouldThrow = connectThrows
        calls.append(.connect(join, media, iceServers))
        if holdConnect {
            holdConnect = false
            await withCheckedContinuation { heldConnect = $0 }
        }
        if shouldThrow { throw Failure.unavailable }
    }

    func disconnect() async { calls.append(.disconnect) }

    func releaseConnect() {
        holdConnect = false
        heldConnect?.resume()
        heldConnect = nil
    }
}
