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
    var isMicrophoneOn: Bool { picture.isMicrophoneOn }
    var isCameraOn: Bool { picture.isCameraOn }
    private(set) var isMicrophoneBlocked = false
    private(set) var isCameraBlocked = false
    /// Counted so that a notice someone put away comes back when the device is refused again.
    private(set) var mediaRefusals: [CallMediaIssue.Kind: Int] = [:]
    var isScreenShareOn: Bool { picture.isScreenShareOn }
    var isScreenShareBlocked: Bool { screenShare.isScreenShareBlocked }
    var isScreenShareStarting: Bool { screenShare.isScreenShareStarting }
    var didScreenShareFail: Bool { screenShare.didScreenShareFail }
    var participants: [CallMediaParticipant] { picture.participants }
    var activeSpeakersLoudestFirst: [String] { picture.activeSpeakersLoudestFirst }
    var connectionQuality: CallConnectionQuality { picture.connectionQuality }
    /// Holds back the rating of a connection that dropped (`CallConnectionRatingGate`).
    @ObservationIgnored private var connectionRating = CallConnectionRatingGate()
    @ObservationIgnored private var connectionSettling: Task<Void, Never>?
    var dockCameraIdentities: CallDockCameraIdentities { picture.dockCameraIdentities }
    /// LiveKit can briefly clear our identity during a full reconnect.
    @ObservationIgnored private var joinedIdentity: String?
    @ObservationIgnored private let remoteVolumes: CallRemoteVolumes
    /// What the person last asked of each device in this call. It outlives
    /// the room, so a reconnect or a replacement session does not unmute
    /// them or turn their camera back on.
    @ObservationIgnored private var captureChoices: [AVMediaType: Bool] = [:]
    /// What the call in progress agreed to start with, for a device the person has not chosen for.
    @ObservationIgnored private var agreedMedia: CallMedia?
    /// The camera is meant to be on: their choice, or failing one what the call agreed.
    private var wantsCamera: Bool { captureChoices[.video] ?? agreedMedia?.video ?? false }
    /// The person chose to mute in this call, whether or not the microphone
    /// is publishing yet. What the system's call screen shows as muted.
    private(set) var isMicrophoneMutedByChoice = false
    func video(of identity: String, source: CallVideoSource) -> AnyView? {
        picture.video(of: identity, source: source, in: roomAccess.room, joinedIdentity: joinedIdentity)
    }

    func setVolume(_ gain: Double, of identity: String) {
        if remoteVolumes.setVolume(gain, of: identity, joinedIdentity: joinedIdentity) {
            refreshParticipants()
        }
    }

    var hasChangedVolumes: Bool { remoteVolumes.hasChangedVolumes }

    func resetVolumes() {
        remoteVolumes.resetVolumes()
        refreshParticipants()
    }

    @ObservationIgnored private let roomAccess = CallRoomAccess()
    @ObservationIgnored private let picture = CallRoomPicture()
    @ObservationIgnored private let videoMeter: CallVideoMeter
    @ObservationIgnored private let screenShare: CallScreenShare
    /// The room holds its delegates weakly.
    @ObservationIgnored private var observer: RoomObserver?

    // MARK: - Devices

    @ObservationIgnored private lazy var deviceCatalog = CallDeviceCatalog(defaults: defaults) { [weak self] in
        guard let self else { return }
        await camera.applyCameraChoice { [weak self] in self?.captureChoices[.video] }
    }
    var devices: CallDevices { deviceCatalog.devices }
    @ObservationIgnored private let defaults: UserDefaults
    /// Consent, cameras and screens: all the controller has of the hardware.
    @ObservationIgnored private let capture: any CallCaptureSources
    @ObservationIgnored private lazy var camera = CallCamera(roomAccess: roomAccess, capture: capture, defaults: defaults) { [weak self] in
        self?.devices.cameras.chosen?.id
    }
    var background: CallBackground { camera.background }
    var isBackgroundFailing: Bool { camera.isBackgroundFailing }
    @ObservationIgnored private let microphoneProcessing: CallMicrophoneProcessing
    var audioProcessing: CallAudioProcessing { microphoneProcessing.audioProcessing }

    init(defaults: UserDefaults = .standard, capture: any CallCaptureSources = SystemCaptureSources()) {
        self.defaults = defaults
        self.capture = capture
        remoteVolumes = CallRemoteVolumes(roomAccess: roomAccess)
        microphoneProcessing = CallMicrophoneProcessing(roomAccess: roomAccess, defaults: defaults)
        videoMeter = CallVideoMeter(roomAccess: roomAccess)
        screenShare = CallScreenShare(roomAccess: roomAccess, capture: capture)
        _ = camera
        roomAccess.tracksChanged = { [weak self] in self?.refreshTracks() }
        roomAccess.recordRefusal = { [weak self] kind in self?.mediaRefusals[kind, default: 0] += 1 }
        // Install the device observers as soon as the controller is created.
        _ = deviceCatalog
    }

    func refreshDevices() async {
        await deviceCatalog.refreshDevices()
    }

    func chooseDevice(_ kind: CallDeviceKind, id: String?) async {
        await deviceCatalog.chooseDevice(kind, id: id)
    }

    // MARK: - Diagnostics

    func setMeasuresVideo(_ on: Bool) async {
        await videoMeter.setMeasuresVideo(on, videoStreams: { self.videoStreams })
    }

    func videoSamples() async -> [CallVideoSample] {
        await videoMeter.videoSamples(videoStreams: { self.videoStreams })
    }

    private var videoStreams: [CallVideoMeter.VideoStream] {
        picture.videoStreams(in: roomAccess.room, joinedIdentity: joinedIdentity)
    }

    // MARK: - Camera background

    func setBackground(_ background: CallBackground) {
        camera.setBackground(background)
    }

    func restartBackground() async {
        await camera.restartBackground(wantsCamera: { wantsCamera }) { room in
            await self.setSource(.video, enabled: true, in: room)
        }
    }

    // MARK: - Audio processing

    func setAudioProcessing(_ processing: CallAudioProcessing) async {
        await microphoneProcessing.setAudioProcessing(processing)
    }

    var appliedAudioProcessing: CallAudioProcessing? { microphoneProcessing.appliedAudioProcessing }

    var microphonePublicationID: String? { picture.microphonePublicationID(in: roomAccess.room) }

    func connect(join: CallJoin, media: CallMedia, iceServers: [WaddleKit.IceServer]) async throws {
        guard roomAccess.room == nil else { throw CallMediaError.alreadyConnected }
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
        roomAccess.room = room
        self.observer = observer
        joinedIdentity = join.identity
        agreedMedia = media
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
            guard roomAccess.holds(room) else { throw CancellationError() }
        } catch {
            if roomAccess.holds(room) { forgetRoom() }
            await room.disconnect()
            camera.releaseBackgroundProcessors(of: room)
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
        guard let room = roomAccess.room else { return }
        forgetRoom()
        await room.disconnect()
        camera.releaseBackgroundProcessors(of: room)
    }

    func forgetCaptureChoices() {
        captureChoices = [:]
        if isMicrophoneMutedByChoice { isMicrophoneMutedByChoice = false }
        remoteVolumes.resetCallChoices()
        refreshParticipants()
    }

    func setMicrophone(_ on: Bool) async {
        captureChoices[.audio] = on
        if isMicrophoneMutedByChoice == on { isMicrophoneMutedByChoice = !on }
        guard let room = roomAccess.room else { return }
        await setSource(.audio, enabled: on, in: room)
    }

    func setCamera(_ on: Bool) async {
        captureChoices[.video] = on
        if !on { camera.cameraNoLongerWanted() }
        guard let room = roomAccess.room else { return }
        await setSource(.video, enabled: on, in: room)
    }

    // MARK: - Sharing our screen

    var canShareScreen: Bool { screenShare.canShareScreen }

    func screenSources() async -> CallScreenSources? {
        await screenShare.screenSources()
    }

    func shareScreen(_ source: CallScreenSource) async {
        await screenShare.shareScreen(source)
    }

    func stopScreenShare() async {
        await screenShare.stopScreenShare()
    }

    /// Starts what the call agreed to, unless the person has since chosen
    /// otherwise for that device.
    private func publish(_ media: CallMedia, in room: Room) async {
        // Here and not before the room exists: a hang-up must find a room to
        // disconnect from the moment its connect starts.
        await refreshDevices()
        guard roomAccess.holds(room) else { return }
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
            _ = try? await room.localParticipant.setMicrophone(enabled: enabled, captureOptions: enabled ? microphoneProcessing.audioCaptureOptions : nil)
        } else {
            if enabled { await camera.turnCameraOn(in: room) }
        }
        // The SDK toggles one publication per device. Off means all of them.
        // And a device turned off while it was still starting is off: the
        // choice as it stands now, not as it stood when the start was asked
        // for, as when Space held to talk came up before the microphone had opened.
        if !enabled || captureChoices[kind] == false {
            for publication in room.callPublications(of: kind) where !publication.isMuted {
                try? await publication.mute()
            }
        }
        // The call ended while a track was starting: `disconnect()` ran
        // before the track was published, so release it here.
        guard roomAccess.holds(room) else {
            await room.localParticipant.unpublishAll()
            await room.disconnect()
            camera.releaseBackgroundProcessors(of: room)
            return
        }
        refreshTracks()
        // The camera was started with the choice as it was when it was asked
        // for; a choice made while it was starting applies now that it is on.
        if kind == .video, enabled {
            await camera.applyCameraChoice(droppingIdleTracks: false) { [weak self] in self?.captureChoices[.video] }
        }
        // Likewise a change to the processing made while the microphone was starting.
        if kind == .audio, enabled { await microphoneProcessing.applyAudioProcessing() }
    }

    /// Asks for the permission, but only for a call that still owns `room`,
    /// and checks again once the user has answered: a prompt can stay open
    /// long after a hang-up. An ended call neither prompts nor captures.
    private func mayCapture(_ kind: AVMediaType, in room: Room) async -> Bool {
        guard roomAccess.holds(room) else { return false }
        let isAllowed = await capture.requestAccess(for: kind)
        // A prompt can be answered long after the call that asked has ended:
        // the answer is not the idle state's, nor the next call's.
        guard roomAccess.holds(room) else { return false }
        // Kept for the dock, which says why the control does nothing.
        if kind == .audio { isMicrophoneBlocked = !isAllowed } else { isCameraBlocked = !isAllowed }
        if !isAllowed { roomAccess.recordRefusal(kind == .audio ? .microphone : .camera) }
        return isAllowed
    }

    private func forgetRoom() {
        let departing = roomAccess.room
        roomAccess.room = nil
        observer = nil
        joinedIdentity = nil
        agreedMedia = nil
        remoteVolumes.roomDidClose()
        phase = .idle
        connectedSince = nil
        mediaUpSince = nil
        // What a call could not use was that call's: the next has not been
        // asked for a camera yet, and may be allowed one by now.
        if isMicrophoneBlocked { isMicrophoneBlocked = false }
        if isCameraBlocked { isCameraBlocked = false }
        if !mediaRefusals.isEmpty { mediaRefusals = [:] }
        screenShare.resetRefusals()
        connectionSettling?.cancel()
        connectionRating = CallConnectionRatingGate()
        microphoneProcessing.roomDidGoAway()
        camera.roomWasForgotten(departing)
        videoMeter.roomWentAway()
        screenShare.roomDidGoAway()
        refreshTracks()
    }

    /// Only the room still in use changes the call. `disconnect()` forgets
    /// the room first, so only a drop the SDK reports on the room still in
    /// use ends the call.
    private func handle(_ change: RoomChange, in changed: Room) {
        guard roomAccess.holds(changed) else { return }
        switch change {
        case .dropped(let endedByServer):
            wasEndedByServer = endedByServer
            onFailure?()
        case .reconnecting:
            connectionSettling?.cancel()
            connectionRating.reconnecting()
            phase = .reconnecting
            refreshParticipants()
        case .reconnected:
            // A reconnect that failed reports its end too, after the drop.
            guard changed.connectionState == .connected else { return }
            phase = .connected
            mediaUpSince = .now
            reconcileTracks(in: changed, restoringVolumes: true)
            // The SDK reports a rating only when it changes: one that is the
            // same on the new connection is shown once that has settled.
            connectionSettling?.cancel()
            connectionSettling = Task { [weak self] in
                try? await Task.sleep(for: CallConnectionRatingGate.settleTime)
                guard !Task.isCancelled, let self, self.roomAccess.holds(changed) else { return }
                self.connectionRating.settled()
                self.refreshParticipants()
            }
            microphoneProcessing.roomDidReconnect()
        case .tracks:
            reconcileTracks(in: changed)
        case .participants:
            refreshParticipants()
        case .ownConnectionRated:
            connectionSettling?.cancel()
            connectionRating.rated()
            refreshParticipants()
        }
    }

    private func reconcileTracks(in room: Room, restoringVolumes: Bool = false) {
        refreshTracks(restoringVolumes: restoringVolumes)
        dropDuplicatePublications(in: room)
        enforceCaptureChoices(in: room)
        // A camera just turned on, or published again after a reconnect, has a capturer that has not been given it.
        camera.giveBackgroundToCameras(in: room)
        microphoneProcessing.tracksDidChange(in: room)
        screenShare.tracksDidChange(in: room)
    }

    /// After a full reconnect the SDK republishes its tracks a moment after
    /// removing them. A device turned on in between is then published
    /// twice, and the controls would only ever reach one of the two.
    private func dropDuplicatePublications(in room: Room) {
        for kind in [AVMediaType.audio, .video] {
            let all = room.callPublications(of: kind)
            guard all.count > 1 else { continue }
            // Keep a live one, so the device stays as the dock shows it.
            let kept = all.first { !$0.isMuted } ?? all[0]
            Task {
                for extra in all where extra !== kept {
                    try? await room.localParticipant.unpublish(publication: extra)
                    if kind == .video { camera.cameraWasDropped(extra, from: room) }
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

    private func refreshTracks(restoringVolumes: Bool = false) {
        let remotes = picture.refreshTracks(from: roomAccess.room, joinedIdentity: joinedIdentity)
        remoteVolumes.tracksChanged(remotes, restoringVolumes: restoringVolumes)
        refreshParticipants()
    }

    private func refreshParticipants() {
        picture.refreshParticipants(
            from: roomAccess.room, phase: phase, connectionRating: connectionRating, gain: remoteVolumes.gain(for:)
        )
    }
}

enum CallMediaError: Error {
    case alreadyConnected
}
