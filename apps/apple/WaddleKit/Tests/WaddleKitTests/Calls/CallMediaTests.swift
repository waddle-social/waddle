import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
@Suite struct CallMediaTests {
    @Test func oldConnectFailureAndCallbackCannotEndReusedSIDAfterClear() async throws {
        let port = FakePort()
        let media = RecordingCallMedia()
        media.holdConnect = true
        media.connectThrows = true
        let store = CallStore(
            port: port, ownJID: { callOwn }, ownBareJID: callOwn.bare,
            media: media, newSid: { "c-reused" })
        #expect(await store.startCall(to: callPeer.bare, media: callAudio))
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-reused", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        let oldFailure = try #require(media.onFailure)
        store.clear()
        await eventually { media.calls.count == 2 }
        media.connectThrows = false
        #expect(await store.startCall(to: callPeer.bare, media: callVideo))
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-reused", kind: .sessionAccept(join: callJoin, media: callVideo)))
        media.releaseConnect()
        await eventually { media.calls.count == 4 }
        oldFailure()
        let carol = JID(parsing: "carol@waddle.test/tab")!
        store.handle(CallEvent(from: carol, to: nil, sid: "c-barrier", kind: .propose(callAudio)))
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
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-old", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-new", kind: .propose(callVideo)))
        await eventually { media.calls.count == 2 }
        #expect(store.state == .incoming(IncomingCall(from: callPeer, sid: "c-new", media: callVideo, isAccepting: true)))
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect])
        let migratedJoin = CallJoin(
            url: "wss://livekit.waddle.test", room: "migrated-room", identity: "alice@waddle.test/waddle-apple-1", token: "new-jwt")
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-new", kind: .sessionInitiate(join: migratedJoin, media: callVideo)))
        await eventually { media.calls.count == 3 }
        #expect(media.calls == [.connect(callJoin, callAudio), .disconnect, .connect(migratedJoin, callVideo)])
        #expect(store.state == .active(ActiveCall(peer: callPeer, sid: "c-new", media: callVideo, join: migratedJoin, initiator: callPeer)))
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
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-reused", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        store.clear()
        await eventually { media.calls.count == 2 }
        #expect(await store.startCall(to: callPeer.bare, media: callVideo))
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-reused", kind: .sessionAccept(join: callJoin, media: callVideo)))
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
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-slow", kind: .sessionAccept(join: callJoin, media: callAudio)))
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
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-media", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        media.onFailure?()
        await eventually { port.callSends.count == 3 && media.calls.count == 2 }
        #expect(store.state == .idle)
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
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-media", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { port.callSends.count == 3 }
        #expect(store.state == .idle)
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
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-media", kind: .sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-media", kind: .sessionTerminate(reason: .success)))
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
        store.handle(CallEvent(from: callPeer, to: nil, sid: "c-media", kind: .sessionAccept(join: callJoin, media: callVideo)))
        await eventually { media.calls == [.connect(callJoin, callVideo)] }
        #expect(media.calls == [.connect(callJoin, callVideo)])
        store.clear()
    }
}

@MainActor private final class RecordingCallMedia: CallMediaController {
    enum Call: Equatable {
        case connect(CallJoin, CallMedia)
        case disconnect
    }
    var onFailure: (@MainActor () -> Void)?
    var calls: [Call] = []
    var connectThrows = false
    var holdConnect = false
    private var heldConnect: CheckedContinuation<Void, Never>?
    private enum Failure: Error { case unavailable }

    func connect(join: CallJoin, media: CallMedia) async throws {
        let shouldThrow = connectThrows
        calls.append(.connect(join, media))
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
