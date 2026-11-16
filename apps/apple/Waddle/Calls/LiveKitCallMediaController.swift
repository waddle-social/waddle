import AVFoundation
import LiveKit
import WaddleKit

/// `CallMediaController` over the LiveKit Swift SDK: one `Room` per call,
/// joined with the credentials the server put in the Jingle transport.
///
/// The SDK is pinned in `project.yml`; bump it under the LiveKit
/// version-currency policy (#1533).
@MainActor
final class LiveKitCallMediaController: CallMediaController {
    var onFailure: (@MainActor () -> Void)?

    /// The room of the call in progress, from the moment its connect starts.
    private var room: Room?
    /// The room holds its delegates weakly.
    private var observer: RoomObserver?

    func connect(join: CallJoin, media: CallMedia, iceServers: [WaddleKit.IceServer]) async throws {
        guard room == nil else { throw CallMediaError.alreadyConnected }
        let observer = RoomObserver { [weak self] room in self?.roomDropped(room) }
        let room = Room(delegate: observer)
        // Owned before connecting, so a drop the SFU reports while the
        // connect is still returning is not lost.
        self.room = room
        self.observer = observer
        // XEP-0215: an advertised list is the ICE set to use. An empty list
        // means no advertisement, and the SDK keeps the servers the SFU
        // hands out during signalling.
        let options = ConnectOptions(
            iceServers: iceServers.map { LiveKit.IceServer(urls: [$0.url], username: $0.username, credential: $0.credential) }
        )
        do {
            try await room.connect(url: join.url, token: join.token, connectOptions: options)
            // A hang-up or a drop that raced the connect owns the slot now.
            try Task.checkCancellation()
            guard self.room === room else { throw CancellationError() }
        } catch {
            if self.room === room { forgetRoom() }
            await room.disconnect()
            throw error
        }
        // Publishing is best effort and separate from joining: a missing
        // device or a denied permission leaves the user in the call,
        // receive-only, as on web and Android. It runs on its own so a
        // permission prompt neither delays the call nor outlives it.
        Task { await publish(media, in: room) }
    }

    func disconnect() async {
        guard let room else { return }
        forgetRoom()
        await room.disconnect()
    }

    /// Asks for each permission first and starts a track only while the call
    /// still owns `room`: a prompt can stay open long after a hang-up, and
    /// the microphone must not start then.
    private func publish(_ media: CallMedia, in room: Room) async {
        if media.audio, await AVCaptureDevice.requestAccess(for: .audio), self.room === room {
            _ = try? await room.localParticipant.setMicrophone(enabled: true)
        }
        if media.video, await AVCaptureDevice.requestAccess(for: .video), self.room === room {
            _ = try? await room.localParticipant.setCamera(enabled: true)
        }
        // The call ended while a track was starting: `disconnect()` ran
        // before the track was published, so release it here.
        if self.room !== room {
            await room.localParticipant.unpublishAll()
            await room.disconnect()
        }
    }

    private func forgetRoom() {
        room = nil
        observer = nil
    }

    /// `disconnect()` forgets the room first, so only a drop the SDK reports
    /// on the room still in use ends the call.
    private func roomDropped(_ dropped: Room) {
        guard dropped === room else { return }
        onFailure?()
    }
}

enum CallMediaError: Error {
    case alreadyConnected
}

/// Forwards the one room event the call slot needs: the media session died
/// after it was established.
private final class RoomObserver: NSObject, RoomDelegate, Sendable {
    private let onDrop: @MainActor (Room) -> Void

    init(onDrop: @escaping @MainActor (Room) -> Void) {
        self.onDrop = onDrop
    }

    func room(_ room: Room, didDisconnectWithError _: LiveKitError?) {
        Task { @MainActor in onDrop(room) }
    }
}
