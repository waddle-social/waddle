import Dispatch
import LiveKit
import Observation
import WaddleKit

@MainActor
final class CallRemoteVolumes {
    private let roomAccess: CallRoomAccess
    @ObservationIgnored private var remoteAudioTracks: [String: [RemoteAudioTrack]] = [:]
    /// Choices follow the call even if LiveKit replaces its room or tracks.
    @ObservationIgnored private var volumeGains: [String: Double] = [:]
    @ObservationIgnored private let volumeUpdates = CallVolumeUpdates()

    init(roomAccess: CallRoomAccess) {
        self.roomAccess = roomAccess
    }

    /// Changes with `participants`, which every volume change refreshes.
    var hasChangedVolumes: Bool { volumeGains.values.contains { $0 != 1 } }

    func gain(for identity: String) -> Double {
        volumeGains[identity] ?? 1
    }

    /// Returns whether the roster needs to be refreshed.
    func setVolume(_ gain: Double, of identity: String, joinedIdentity: String?) -> Bool {
        guard let room = roomAccess.room, !identity.isEmpty, identity != joinedIdentity,
              identity != room.localParticipant.identity?.stringValue else { return false }
        let gain = CallVolume.clamp(gain)
        volumeGains[identity] = gain
        for track in remoteAudioTracks[identity] ?? [] { applyVolume(gain, to: track) }
        return true
    }

    func resetVolumes() {
        for identity in volumeGains.keys {
            for track in remoteAudioTracks[identity] ?? [] { applyVolume(1, to: track) }
        }
        volumeGains.removeAll()
    }

    func resetCallChoices() {
        volumeUpdates.removeAll()
        resetVolumes()
    }

    func roomDidClose() {
        volumeUpdates.removeAll()
    }

    func tracksChanged(_ remotes: [RemoteParticipant], restoringVolumes: Bool) {
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

    private func applyVolume(_ gain: Double, to track: RemoteAudioTrack) {
        volumeUpdates.enqueue(for: track) { track.volume = gain }
    }
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
