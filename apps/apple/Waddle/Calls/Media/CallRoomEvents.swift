import Foundation
import LiveKit

/// What the call needs to hear from a room.
enum RoomChange {
    /// The media session died after it was established. True when the
    /// SFU ended it on purpose: this participant was removed, the room was
    /// closed, or the same identity joined from elsewhere.
    case dropped(endedByServer: Bool)
    case reconnecting
    case reconnected
    /// A track was published, subscribed, muted or removed, on either side.
    case tracks
    /// Someone joined, or the SDK reported who is speaking and how loudly.
    case participants
    /// The server rated our own connection.
    case ownConnectionRated
}

/// Hands room events to the controller as an ordered stream. The
/// reconnect-mode callbacks are used because a quick reconnect changes
/// neither `connectionState` nor `roomIsReconnecting`.
final class RoomObserver: NSObject, RoomDelegate, Sendable {
    private let changes: AsyncStream<RoomChange>.Continuation

    static func make() -> (AsyncStream<RoomChange>, RoomObserver) {
        let (stream, continuation) = AsyncStream<RoomChange>.makeStream()
        return (stream, RoomObserver(changes: continuation))
    }

    private init(changes: AsyncStream<RoomChange>.Continuation) {
        self.changes = changes
    }

    deinit {
        changes.finish()
    }

    private func forward(_ change: RoomChange, from _: Room) {
        changes.yield(change)
    }

    func room(_ room: Room, didDisconnectWithError error: LiveKitError?) {
        let endedByServer: Bool
        switch error?.type {
        case .participantRemoved?, .roomDeleted?, .duplicateIdentity?: endedByServer = true
        default: endedByServer = false
        }
        forward(.dropped(endedByServer: endedByServer), from: room)
    }

    func room(_ room: Room, didStartReconnectWithMode _: ReconnectMode) {
        forward(.reconnecting, from: room)
    }

    func room(_ room: Room, didCompleteReconnectWithMode _: ReconnectMode) {
        forward(.reconnected, from: room)
    }

    func room(_ room: Room, participant _: LocalParticipant, didPublishTrack _: LocalTrackPublication) {
        forward(.tracks, from: room)
    }

    func room(_ room: Room, participant _: LocalParticipant, didUnpublishTrack _: LocalTrackPublication) {
        forward(.tracks, from: room)
    }

    func room(_ room: Room, participant _: RemoteParticipant, didSubscribeTrack _: RemoteTrackPublication) {
        forward(.tracks, from: room)
    }

    func room(_ room: Room, participant _: RemoteParticipant, didUnsubscribeTrack _: RemoteTrackPublication) {
        forward(.tracks, from: room)
    }

    func room(_ room: Room, participant _: RemoteParticipant, trackPublication _: RemoteTrackPublication, didUpdateIsSubscriptionAllowed _: Bool) {
        forward(.tracks, from: room)
    }

    func room(_ room: Room, participant _: Participant, trackPublication _: TrackPublication, didUpdateIsMuted _: Bool) {
        forward(.tracks, from: room)
    }

    func room(_ room: Room, participantDidDisconnect _: RemoteParticipant) {
        forward(.tracks, from: room)
    }

    func room(_ room: Room, participantDidConnect _: RemoteParticipant) {
        forward(.participants, from: room)
    }

    func room(_ room: Room, didUpdateSpeakingParticipants _: [Participant]) {
        forward(.participants, from: room)
    }

    func room(_ room: Room, participant: Participant, didUpdateConnectionQuality _: ConnectionQuality) {
        // Only ours is shown.
        if participant is LocalParticipant { forward(.ownConnectionRated, from: room) }
    }
}
