import AVFoundation
import LiveKit
import Observation
import SwiftUI
import WaddleKit

/// Owns the roster, video tracks and published device state shown for a room.
///
/// Each refresh must read the room whole, in one synchronous pass without an await.
/// Publish only a changed value. Every assignment to an observed property
/// invalidates SwiftUI views, and a needless redraw starves the video (#2625).
@MainActor
@Observable
final class CallRoomPicture {
    private(set) var isMicrophoneOn = false
    private(set) var isCameraOn = false
    private(set) var isScreenShareOn = false
    private(set) var participants: [CallMediaParticipant] = []
    private(set) var activeSpeakersLoudestFirst: [String] = []
    private(set) var connectionQuality = CallConnectionQuality.unknown
    private(set) var dockCameraIdentities = CallDockCameraIdentities()
    /// Our own shared screen, for the notice that shows us what everyone sees.
    private var ownScreenTrack: VideoTrack?
    private var cameraTracks: [String: VideoTrack] = [:]
    /// The screens other people share, by who shares them.
    private var screenTracks: [String: SharedScreen] = [:]

    func video(of identity: String, source: CallVideoSource, in room: Room?, joinedIdentity: String?) -> AnyView? {
        switch source {
        case .camera:
            guard let track = cameraTracks[identity] else { return nil }
            // A remote portrait camera keeps the whole face.
            return AnyView(SwiftUIVideoView(track, layoutMode: identity == dockCameraIdentities.local ? .fill : .fit))
        case .screen:
            // All of a screen, never cropped.
            let own = room?.localParticipant.identity?.stringValue ?? joinedIdentity
            let track = identity == own ? ownScreenTrack : screenTracks[identity]?.track
            return track.map { AnyView(SwiftUIVideoView($0, layoutMode: .fit)) }
        }
    }

    /// Every video stream of the call now: ours first, then the others'.
    func videoStreams(in room: Room?, joinedIdentity: String?) -> [CallVideoMeter.VideoStream] {
        guard let room else { return [] }
        let own: String = room.localParticipant.identity?.stringValue ?? joinedIdentity ?? ""
        var streams: [CallVideoMeter.VideoStream] = []
        for publication in room.localParticipant.localVideoTracks where !publication.isMuted {
            guard let track = publication.track else { continue }
            let source: CallVideoSource = publication.source == .screenShareVideo ? .screen : .camera
            streams.append(.init(key: "\(own):\(source):sent", label: "You", source: source, direction: .sent, track: track))
        }
        for (identity, camera) in cameraTracks.sorted(by: { $0.key < $1.key }) where identity != own {
            guard let track = camera as? Track else { continue }
            streams.append(.init(
                key: "\(identity):camera:received", label: Self.name(of: identity), source: .camera, direction: .received,
                track: track
            ))
        }
        for (identity, screen) in screenTracks.sorted(by: { $0.key < $1.key }) {
            guard let track = screen.track as? Track else { continue }
            streams.append(.init(
                key: "\(identity):screen:received", label: Self.name(of: identity), source: .screen, direction: .received,
                track: track
            ))
        }
        return streams
    }

    private static func name(of identity: String) -> String {
        JID(parsing: identity)?.bare.localpart ?? identity
    }

    /// The microphone's publication, for tests: a change of processing must not replace it.
    func microphonePublicationID(in room: Room?) -> String? {
        room.flatMap { $0.callPublications(of: .audio).first?.sid.stringValue }
    }

    /// Reads the room's tracks as they are now, so the order in which the
    /// SDK reports changes cannot leave a stale picture.
    func refreshTracks(from room: Room?, joinedIdentity: String?) -> [RemoteParticipant] {
        // On if any publication of the device is live, so a second one can
        // never send while the dock shows the device off.
        let preview = room.flatMap { $0.callPublications(of: .video).first(where: \.isLive)?.track as? VideoTrack }
        let microphone = room.map { $0.callPublications(of: .audio).contains(where: \.isLive) } ?? false
        let camera = preview != nil
        let ownScreen = room?.localParticipant.localVideoTracks
            .first { $0.source == .screenShareVideo && !$0.isMuted }?.track as? VideoTrack
        var tracks: [String: VideoTrack] = [:]
        var screens: [String: SharedScreen] = [:]
        var dockCameras = CallDockCameraIdentities()
        if let preview, let local = room?.localParticipant.identity?.stringValue ?? joinedIdentity {
            tracks[local] = preview
            dockCameras.local = local
        }
        let remotes = room.map { Array($0.remoteParticipants.values) } ?? []
        for remote in remotes {
            guard let identity = remote.identity?.stringValue else { continue }
            if let publication = remote.videoTracks.first(where: { $0.source == .screenShareVideo && $0.isLive }),
               let track = publication.track as? VideoTrack {
                screens[identity] = SharedScreen(track: track, publicationID: publication.sid.stringValue)
            }
            // A camera that is off shows no picture rather than its last frame.
            guard remote.isCameraEnabled(), let track = remote.firstCameraVideoTrack else { continue }
            tracks[identity] = track
            // Keep the dock's original choice from the room map. The roster
            // has a different order so people don't move around while talking.
            if dockCameras.remote == nil { dockCameras.remote = identity }
        }
        if isMicrophoneOn != microphone { isMicrophoneOn = microphone }
        if isCameraOn != camera { isCameraOn = camera }
        if ownScreenTrack !== ownScreen { ownScreenTrack = ownScreen }
        if isScreenShareOn != (ownScreen != nil) { isScreenShareOn = ownScreen != nil }
        // A replacement can reuse a WebRTC track ID but needs a new renderer binding.
        if cameraTracks.count != tracks.count || tracks.contains(where: { cameraTracks[$0.key] !== $0.value }) {
            cameraTracks = tracks
        }
        // The dock's one screen is the first in a fixed order, so it does not
        // hop between two sharers as people come and go.
        dockCameras.remoteScreen = screens.keys.min()
        if screenTracks.count != screens.count || screens.contains(where: { screenTracks[$0.key] != $0.value }) {
            screenTracks = screens
        }
        if dockCameraIdentities != dockCameras { dockCameraIdentities = dockCameras }
        return remotes
    }

    /// Reads who is in the room, and who of them is speaking, as they are
    /// now; like `refreshTracks`, never applied as deltas. Only a connected
    /// room has a roster: a reconnect may replace every participant.
    func refreshParticipants(
        from room: Room?, phase: CallMediaPhase, connectionRating: CallConnectionRatingGate,
        gain: @MainActor (String) -> Double
    ) {
        guard let room, phase == .connected else {
            if !participants.isEmpty { participants = [] }
            if !activeSpeakersLoudestFirst.isEmpty { activeSpeakersLoudestFirst = [] }
            // What was measured belonged to a connection that is gone or being remade.
            if connectionQuality != .unknown { connectionQuality = .unknown }
            return
        }
        let rated: CallConnectionQuality = switch room.localParticipant.connectionQuality {
        case .excellent: .excellent
        case .good: .good
        case .poor: .poor
        case .lost: .lost
        case .unknown: .unknown
        }
        let quality = connectionRating.shown(rated)
        if connectionQuality != quality { connectionQuality = quality }
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
                screenPublicationID: screenTracks[identity]?.publicationID,
                hasMicrophoneTrack: participant.audioTracks.contains { $0.source == .microphone && $0.isLive },
                isSpeaking: participant.isSpeaking, volume: isLocal ? 1 : gain(identity)
            )
        }
        if participants != rows { participants = rows }
        // Levels change many times a second; only a change of who speaks,
        // or of their order, is published.
        let speakers = SpeakerPromotion.activeLoudestFirst(everyone.compactMap { participant in
            participant.identity.map { ($0.stringValue, participant.isSpeaking, participant.audioLevel) }
        })
        if activeSpeakersLoudestFirst != speakers { activeSpeakersLoudestFirst = speakers }
    }
}

extension Room {
    /// Every publication of one of our devices. There is normally one.
    @MainActor
    func callPublications(of kind: AVMediaType) -> [LocalTrackPublication] {
        let local = localParticipant
        return kind == .audio
            ? local.localAudioTracks.filter { $0.source == .microphone }
            : local.localVideoTracks.filter { $0.source == .camera }
    }
}

/// A screen someone shares: its picture, and the publication that tells one share from the next.
private struct SharedScreen: Equatable {
    let track: VideoTrack
    let publicationID: String

    /// A replacement can reuse a publication but needs a new renderer binding.
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.track === rhs.track && lhs.publicationID == rhs.publicationID
    }
}

private extension TrackPublication {
    var isLive: Bool { isSubscribed && !isMuted }
}
