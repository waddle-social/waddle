import AVFoundation
import LiveKit
import SwiftUI
import WaddleKit

/// `CallMediaController` over the LiveKit Swift SDK: one `Room` per call,
/// joined with the credentials the server put in the Jingle transport.
///
/// The SDK is pinned in `project.yml`; bump it under the LiveKit
/// version-currency policy (#1533).
@MainActor
@Observable
final class LiveKitCallMediaController: CallMediaController, CallMediaControls {
    @ObservationIgnored var onFailure: (@MainActor () -> Void)?

    private(set) var phase: CallMediaPhase = .idle
    private(set) var connectedSince: Date?
    private(set) var isMicrophoneOn = false
    private(set) var isCameraOn = false
    private(set) var isMicrophoneBlocked = false
    private(set) var isCameraBlocked = false
    /// What the person last asked of each device in this call. It outlives
    /// the room, so a reconnect or a replacement session does not unmute
    /// them or turn their camera back on.
    @ObservationIgnored private var captureChoices: [AVMediaType: Bool] = [:]
    private var remoteTrack: VideoTrack?
    private var localTrack: VideoTrack?

    /// Fitted, not filled: a portrait phone camera keeps the whole face.
    var remoteVideo: AnyView? { remoteTrack.map { AnyView(SwiftUIVideoView($0, layoutMode: .fit)) } }
    var localVideo: AnyView? { localTrack.map { AnyView(SwiftUIVideoView($0)) } }

    /// The room of the call in progress, from the moment its connect starts.
    @ObservationIgnored private var room: Room?
    /// The room holds its delegates weakly.
    @ObservationIgnored private var observer: RoomObserver?

    func connect(join: CallJoin, media: CallMedia, iceServers: [WaddleKit.IceServer]) async throws {
        guard room == nil else { throw CallMediaError.alreadyConnected }
        let (changes, observer) = RoomObserver.make()
        let room = Room(delegate: observer)
        // One consumer, so the room's reports are handled in the order the
        // SDK made them. It ends when the observer is released.
        Task { [weak self] in
            for await change in changes { self?.handle(change, in: room) }
        }
        // Owned before connecting, so a drop the SFU reports while the
        // connect is still returning is not lost.
        self.room = room
        self.observer = observer
        phase = .connecting
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
        if phase == .connecting { phase = .connected }
        connectedSince = .now
        refreshTracks()
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

    func forgetCaptureChoices() {
        captureChoices = [:]
    }

    func setMicrophone(_ on: Bool) async {
        captureChoices[.audio] = on
        guard let room else { return }
        await setSource(.audio, enabled: on, in: room)
    }

    func setCamera(_ on: Bool) async {
        captureChoices[.video] = on
        guard let room else { return }
        await setSource(.video, enabled: on, in: room)
    }

    /// Starts what the call agreed to, unless the person has since chosen
    /// otherwise for that device.
    private func publish(_ media: CallMedia, in room: Room) async {
        if captureChoices[.audio] ?? media.audio { await setSource(.audio, enabled: true, in: room) }
        if captureChoices[.video] ?? media.video { await setSource(.video, enabled: true, in: room) }
    }

    /// Turning a device on goes through `mayCapture`; turning one off
    /// needs no permission.
    private func setSource(_ kind: AVMediaType, enabled: Bool, in room: Room) async {
        if enabled, !(await mayCapture(kind, in: room)) { return }
        // The person changed their mind while a prompt or a start was pending.
        if enabled, captureChoices[kind] == false { return }
        if kind == .audio {
            _ = try? await room.localParticipant.setMicrophone(enabled: enabled)
        } else {
            _ = try? await room.localParticipant.setCamera(enabled: enabled)
        }
        // The call ended while a track was starting: `disconnect()` ran
        // before the track was published, so release it here.
        guard self.room === room else {
            await room.localParticipant.unpublishAll()
            await room.disconnect()
            return
        }
        refreshTracks()
    }

    /// Asks for the permission, but only for a call that still owns `room`,
    /// and checks again once the user has answered: a prompt can stay open
    /// long after a hang-up. An ended call neither prompts nor captures.
    private func mayCapture(_ kind: AVMediaType, in room: Room) async -> Bool {
        guard self.room === room else { return false }
        let isAllowed = await AVCaptureDevice.requestAccess(for: kind)
        // Kept for the dock, which says why the control does nothing.
        if kind == .audio { isMicrophoneBlocked = !isAllowed } else { isCameraBlocked = !isAllowed }
        return isAllowed && self.room === room
    }

    private func forgetRoom() {
        room = nil
        observer = nil
        phase = .idle
        connectedSince = nil
        refreshTracks()
    }

    /// Only the room still in use changes the call. `disconnect()` forgets
    /// the room first, so only a drop the SDK reports on the room still in
    /// use ends the call.
    private func handle(_ change: RoomChange, in changed: Room) {
        guard changed === room else { return }
        switch change {
        case .dropped: onFailure?()
        case .reconnecting: phase = .reconnecting
        case .reconnected:
            phase = .connected
            refreshTracks()
            enforceCaptureChoices(in: changed)
        case .tracks:
            refreshTracks()
            enforceCaptureChoices(in: changed)
        }
    }

    /// A reconnect republishes the tracks it had, and a tap during it finds
    /// nothing to mute. Whatever comes back, a device the person turned
    /// off goes off again.
    private func enforceCaptureChoices(in room: Room) {
        if isMicrophoneOn, captureChoices[.audio] == false {
            Task { await setSource(.audio, enabled: false, in: room) }
        }
        if isCameraOn, captureChoices[.video] == false {
            Task { await setSource(.video, enabled: false, in: room) }
        }
    }

    /// Reads the room's tracks as they are now, so the order in which the
    /// SDK reports changes cannot leave a stale picture.
    private func refreshTracks() {
        let local = room?.localParticipant
        let microphone = local?.isMicrophoneEnabled() ?? false
        let camera = local?.isCameraEnabled() ?? false
        // A camera that is off shows no picture rather than its last frame.
        let preview = camera ? local?.firstCameraVideoTrack : nil
        let remote = room?.remoteParticipants.values.lazy
            .filter { $0.isCameraEnabled() }
            .compactMap(\.firstCameraVideoTrack).first
        if isMicrophoneOn != microphone { isMicrophoneOn = microphone }
        if isCameraOn != camera { isCameraOn = camera }
        if localTrack !== preview { localTrack = preview }
        if remoteTrack !== remote { remoteTrack = remote }
    }
}

enum CallMediaError: Error {
    case alreadyConnected
}

/// What the call needs to hear from a room.
private enum RoomChange {
    /// The media session died after it was established.
    case dropped
    case reconnecting
    case reconnected
    /// A track was published, subscribed, muted or removed, on either side.
    case tracks
}

/// Hands room events to the controller as an ordered stream. The
/// reconnect-mode callbacks are used because a quick reconnect changes
/// neither `connectionState` nor `roomIsReconnecting`.
private final class RoomObserver: NSObject, RoomDelegate, Sendable {
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

    func room(_ room: Room, didDisconnectWithError _: LiveKitError?) {
        forward(.dropped, from: room)
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

    func room(_ room: Room, participant _: Participant, trackPublication _: TrackPublication, didUpdateIsMuted _: Bool) {
        forward(.tracks, from: room)
    }

    func room(_ room: Room, participantDidDisconnect _: RemoteParticipant) {
        forward(.tracks, from: room)
    }
}
