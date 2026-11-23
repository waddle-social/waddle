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

    @ObservationIgnored var onPhaseChange: (@MainActor (CallMediaPhase) -> Void)?
    @ObservationIgnored private(set) var wasEndedByServer = false

    private(set) var phase: CallMediaPhase = .idle {
        didSet { if phase != oldValue { onPhaseChange?(phase) } }
    }
    private(set) var connectedSince: Date?
    private(set) var mediaUpSince: Date?
    private(set) var isMicrophoneOn = false
    private(set) var isCameraOn = false
    private(set) var isMicrophoneBlocked = false
    private(set) var isCameraBlocked = false
    private(set) var participants: [CallMediaParticipant] = []
    private(set) var dockCameraIdentities = CallDockCameraIdentities()
    private var cameraTracks: [String: VideoTrack] = [:]
    /// LiveKit can briefly clear our identity during a full reconnect.
    @ObservationIgnored private var joinedIdentity: String?
    @ObservationIgnored private var remoteAudioTracks: [String: [RemoteAudioTrack]] = [:]
    /// Choices follow the call even if LiveKit replaces its room or tracks.
    @ObservationIgnored private var volumeGains: [String: Double] = [:]
    @ObservationIgnored private let volumeUpdates = CallVolumeUpdates()
    /// What the person last asked of each device in this call. It outlives
    /// the room, so a reconnect or a replacement session does not unmute
    /// them or turn their camera back on.
    @ObservationIgnored private var captureChoices: [AVMediaType: Bool] = [:]
    /// The person chose to mute in this call, whether or not the microphone
    /// is publishing yet. What the system's call screen shows as muted.
    private(set) var isMicrophoneMutedByChoice = false
    func video(of identity: String, source: CallVideoSource) -> AnyView? {
        guard source == .camera, let track = cameraTracks[identity] else { return nil }
        // A remote portrait camera keeps the whole face.
        return AnyView(SwiftUIVideoView(track, layoutMode: identity == dockCameraIdentities.local ? .fill : .fit))
    }

    func setVolume(_ gain: Double, of identity: String) {
        guard let room, !identity.isEmpty, identity != joinedIdentity,
              identity != room.localParticipant.identity?.stringValue else { return }
        let gain = CallVolume.clamp(gain)
        volumeGains[identity] = gain
        for track in remoteAudioTracks[identity] ?? [] { applyVolume(gain, to: track) }
        refreshParticipants()
    }

    private func applyVolume(_ gain: Double, to track: RemoteAudioTrack) {
        volumeUpdates.enqueue(for: track) { track.volume = gain }
    }

    /// The room of the call in progress, from the moment its connect starts.
    @ObservationIgnored private var room: Room?
    /// The room holds its delegates weakly.
    @ObservationIgnored private var observer: RoomObserver?

    func connect(join: CallJoin, media: CallMedia, iceServers: [WaddleKit.IceServer]) async throws {
        guard room == nil else { throw CallMediaError.alreadyConnected }
        wasEndedByServer = false
        let (changes, observer) = RoomObserver.make()
        let room = Room(delegate: observer, roomOptions: RoomOptions(adaptiveStream: true, dynacast: true))
        // One consumer, so the room's reports are handled in the order the
        // SDK made them. It ends when the observer is released.
        Task { [weak self] in
            for await change in changes { self?.handle(change, in: room) }
        }
        // Owned before connecting, so a drop the SFU reports while the
        // connect is still returning is not lost.
        self.room = room
        self.observer = observer
        joinedIdentity = join.identity
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
        mediaUpSince = .now
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
        if isMicrophoneMutedByChoice { isMicrophoneMutedByChoice = false }
        volumeUpdates.removeAll()
        for identity in volumeGains.keys {
            for track in remoteAudioTracks[identity] ?? [] { applyVolume(1, to: track) }
        }
        volumeGains.removeAll()
        refreshParticipants()
    }

    func setMicrophone(_ on: Bool) async {
        captureChoices[.audio] = on
        if isMicrophoneMutedByChoice == on { isMicrophoneMutedByChoice = !on }
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
        // The SDK toggles one publication per device. Off means all of them.
        if !enabled {
            for publication in publications(of: kind, in: room) where !publication.isMuted {
                try? await publication.mute()
            }
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
        joinedIdentity = nil
        volumeUpdates.removeAll()
        phase = .idle
        connectedSince = nil
        mediaUpSince = nil
        refreshTracks()
    }

    /// Only the room still in use changes the call. `disconnect()` forgets
    /// the room first, so only a drop the SDK reports on the room still in
    /// use ends the call.
    private func handle(_ change: RoomChange, in changed: Room) {
        guard changed === room else { return }
        switch change {
        case .dropped(let endedByServer):
            wasEndedByServer = endedByServer
            onFailure?()
        case .reconnecting:
            phase = .reconnecting
            refreshParticipants()
        case .reconnected:
            // A reconnect that failed reports its end too, after the drop.
            guard changed.connectionState == .connected else { return }
            phase = .connected
            mediaUpSince = .now
            reconcileTracks(in: changed, restoringVolumes: true)
        case .tracks:
            reconcileTracks(in: changed)
        case .participants:
            refreshParticipants()
        }
    }

    private func reconcileTracks(in room: Room, restoringVolumes: Bool = false) {
        refreshTracks(restoringVolumes: restoringVolumes)
        dropDuplicatePublications(in: room)
        enforceCaptureChoices(in: room)
    }

    /// Every publication of one of our devices. There is normally one.
    private func publications(of kind: AVMediaType, in room: Room) -> [LocalTrackPublication] {
        let local = room.localParticipant
        return kind == .audio
            ? local.localAudioTracks.filter { $0.source == .microphone }
            : local.localVideoTracks.filter { $0.source == .camera }
    }

    /// After a full reconnect the SDK republishes its tracks a moment after
    /// removing them. A device turned on in between is then published
    /// twice, and the controls would only ever reach one of the two.
    private func dropDuplicatePublications(in room: Room) {
        for kind in [AVMediaType.audio, .video] {
            let all = publications(of: kind, in: room)
            guard all.count > 1 else { continue }
            // Keep a live one, so the device stays as the dock shows it.
            let kept = all.first { !$0.isMuted } ?? all[0]
            Task {
                for extra in all where extra !== kept {
                    try? await room.localParticipant.unpublish(publication: extra)
                }
            }
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
    private func refreshTracks(restoringVolumes: Bool = false) {
        // On if any publication of the device is live, so a second one can
        // never send while the dock shows the device off.
        let preview = room.flatMap { publications(of: .video, in: $0).first(where: \.isLive)?.track as? VideoTrack }
        let microphone = room.map { publications(of: .audio, in: $0).contains(where: \.isLive) } ?? false
        let camera = preview != nil
        var tracks: [String: VideoTrack] = [:]
        var dockCameras = CallDockCameraIdentities()
        if let preview, let local = room?.localParticipant.identity?.stringValue ?? joinedIdentity {
            tracks[local] = preview
            dockCameras.local = local
        }
        let remotes = room.map { Array($0.remoteParticipants.values) } ?? []
        for remote in remotes {
            // A camera that is off shows no picture rather than its last frame.
            guard let identity = remote.identity?.stringValue, remote.isCameraEnabled(),
                  let track = remote.firstCameraVideoTrack else { continue }
            tracks[identity] = track
            // Keep the dock's original choice from the room map. The roster
            // has a different order so people don't move around while talking.
            if dockCameras.remote == nil { dockCameras.remote = identity }
        }
        if isMicrophoneOn != microphone { isMicrophoneOn = microphone }
        if isCameraOn != camera { isCameraOn = camera }
        // A replacement can reuse a WebRTC track ID but needs a new renderer binding.
        if cameraTracks.count != tracks.count || tracks.contains(where: { cameraTracks[$0.key] !== $0.value }) {
            cameraTracks = tracks
        }
        if dockCameraIdentities != dockCameras { dockCameraIdentities = dockCameras }
        refreshRemoteAudio(remotes, restoringVolumes: restoringVolumes)
        refreshParticipants()
    }

    private func refreshRemoteAudio(_ remotes: [RemoteParticipant], restoringVolumes: Bool) {
        var subscribed: [String: [RemoteAudioTrack]] = [:]
        for remote in remotes {
            guard let identity = remote.identity?.stringValue else { continue }
            let tracks = remote.audioTracks.filter(\.isSubscribed).compactMap { $0.track as? RemoteAudioTrack }
            guard !tracks.isEmpty else { continue }
            subscribed[identity] = tracks
            guard let gain = volumeGains[identity] else { continue }
            let previous = remoteAudioTracks[identity] ?? []
            // Set gain even on muted tracks so they have it when unmuted.
            for track in tracks where restoringVolumes || !previous.contains(where: { $0 === track }) {
                applyVolume(gain, to: track)
            }
        }
        remoteAudioTracks = subscribed
    }

    /// Reads who is in the room, and who of them is speaking, as they are
    /// now; like `refreshTracks`, never applied as deltas. Only a connected
    /// room has a roster: a reconnect may replace every participant.
    private func refreshParticipants() {
        guard let room, phase == .connected else {
            if !participants.isEmpty { participants = [] }
            return
        }
        // Ours first, then everyone else in the order they joined, so the
        // roster keeps its order from one report to the next.
        let remotes = room.remoteParticipants.values.sorted { lhs, rhs in
            switch (lhs.joinedAt, rhs.joinedAt) {
            case let (left?, right?) where left != right: left < right
            case (.some, nil): true
            case (nil, .some): false
            default: (lhs.identity?.stringValue ?? "") < (rhs.identity?.stringValue ?? "")
            }
        }
        let everyone: [Participant] = [room.localParticipant] + remotes
        // Each participant's own flag, not `room.activeSpeakers`: the SDK
        // hears about speakers over two channels and keeps that list up to
        // date for only one of them.
        let rows = everyone.compactMap { participant -> CallMediaParticipant? in
            guard let identity = participant.identity?.stringValue else { return nil }
            let isLocal = participant === room.localParticipant
            return CallMediaParticipant(
                identity: identity, isLocal: isLocal, isCameraOn: cameraTracks[identity] != nil,
                hasMicrophoneTrack: participant.audioTracks.contains { $0.source == .microphone && $0.isLive },
                isSpeaking: participant.isSpeaking, volume: isLocal ? 1 : volumeGains[identity] ?? 1
            )
        }
        if participants != rows { participants = rows }
    }
}

private extension TrackPublication {
    var isLive: Bool { isSubscribed && !isMuted }
}

/// LiveKit waits on WebRTC's signaling thread for each write. Only one batch
/// goes to a GCD queue at a time so a slow write can't queue up a slider drag.
@MainActor
final class CallVolumeUpdates {
    private let queue = DispatchQueue(label: "waddle.call-volume", qos: .userInitiated)
    private var pending: [ObjectIdentifier: @Sendable () -> Void] = [:]
    private var isFlushing = false

    func enqueue(for track: AnyObject, apply: @escaping @Sendable () -> Void) {
        pending[ObjectIdentifier(track)] = apply
        guard !isFlushing else { return }
        isFlushing = true
        Task { await flush() }
    }

    func removeAll() {
        pending.removeAll()
    }

    private func flush() async {
        defer { isFlushing = false }
        while !pending.isEmpty {
            try? await Task.sleep(for: .milliseconds(40))
            let batch = Array(pending.values)
            pending.removeAll()
            guard !batch.isEmpty else { return }
            await withCheckedContinuation { continuation in
                queue.async {
                    for apply in batch { apply() }
                    continuation.resume()
                }
            }
        }
    }
}

enum CallMediaError: Error {
    case alreadyConnected
}

/// What the call needs to hear from a room.
private enum RoomChange {
    /// The media session died after it was established. True when the
    /// SFU ended it on purpose: this participant was removed, the room was
    /// closed, or the same identity joined from elsewhere.
    case dropped(endedByServer: Bool)
    case reconnecting
    case reconnected
    /// A track was published, subscribed, muted or removed, on either side.
    case tracks
    /// Someone joined, or the set of people speaking changed.
    case participants
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
}
