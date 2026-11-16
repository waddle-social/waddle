import Foundation
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

    private var room: Room?
    /// The room holds its delegates weakly.
    private var observer: RoomObserver?

    func connect(join: CallJoin, media: CallMedia, iceServers: [WaddleKit.IceServer]) async throws {
        guard room == nil else { throw CallMediaError.alreadyConnected }
        let observer = RoomObserver { [weak self] room in self?.roomDropped(room) }
        let room = Room(delegate: observer)
        // XEP-0215: an advertised list is the ICE set to use. An empty list
        // means no advertisement, and the SDK keeps the servers the SFU
        // hands out during signalling.
        let options = ConnectOptions(
            iceServers: iceServers.map { LiveKit.IceServer(urls: [$0.url], username: $0.username, credential: $0.credential) }
        )
        do {
            try await room.connect(url: join.url, token: join.token, connectOptions: options)
            // A hang-up that raced the connect owns the slot now.
            try Task.checkCancellation()
        } catch {
            await room.disconnect()
            throw error
        }
        self.room = room
        self.observer = observer
        // Publishing is best effort and separate from joining: a missing
        // device or a denied permission leaves the user in the call,
        // receive-only, as on web and Android.
        if media.audio { _ = try? await room.localParticipant.setMicrophone(enabled: true) }
        if media.video { _ = try? await room.localParticipant.setCamera(enabled: true) }
    }

    func disconnect() async {
        guard let room else { return }
        self.room = nil
        observer = nil
        await room.disconnect()
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
