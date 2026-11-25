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
    /// Counted so that a notice someone put away comes back when the device is refused again.
    private(set) var mediaRefusals: [CallMediaIssue.Kind: Int] = [:]
    private(set) var isScreenShareOn = false
    private(set) var isScreenShareBlocked = false
    /// Our own shared screen, for the notice that shows us what everyone sees.
    private var ownScreenTrack: VideoTrack?
    /// A pick is being published; the button already offers to stop it.
    private(set) var isScreenShareStarting = false
    /// The last pick could not be shared.
    private(set) var didScreenShareFail = false
    #if os(macOS)
    /// Shares and stops run one after another, in the order asked.
    @ObservationIgnored private var screenShareWork: Task<Void, Never>?
    /// Moves on with every ask, so a share that finishes starting can tell it is no longer wanted.
    @ObservationIgnored private var screenShareGeneration = 0
    @ObservationIgnored private var screenCaptureWatcher: ScreenCaptureWatcher?
    /// What the person last asked: a full reconnect republishes every track
    /// it had, and a screen they stopped meanwhile must not come back.
    @ObservationIgnored private var wantsScreenShare = false
    #endif
    private(set) var participants: [CallMediaParticipant] = []
    private(set) var activeSpeakersLoudestFirst: [String] = []
    private(set) var connectionQuality = CallConnectionQuality.unknown
    /// Holds back the rating of a connection that dropped (`CallConnectionRatingGate`).
    @ObservationIgnored private var connectionRating = CallConnectionRatingGate()
    @ObservationIgnored private var connectionSettling: Task<Void, Never>?
    private(set) var dockCameraIdentities = CallDockCameraIdentities()
    private var cameraTracks: [String: VideoTrack] = [:]
    /// The screens other people share, by who shares them.
    private var screenTracks: [String: SharedScreen] = [:]
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

    func setVolume(_ gain: Double, of identity: String) {
        guard let room, !identity.isEmpty, identity != joinedIdentity,
              identity != room.localParticipant.identity?.stringValue else { return }
        let gain = CallVolume.clamp(gain)
        volumeGains[identity] = gain
        for track in remoteAudioTracks[identity] ?? [] { applyVolume(gain, to: track) }
        refreshParticipants()
    }

    /// Changes with `participants`, which every volume change refreshes.
    var hasChangedVolumes: Bool { volumeGains.values.contains { $0 != 1 } }

    func resetVolumes() {
        for identity in volumeGains.keys {
            for track in remoteAudioTracks[identity] ?? [] { applyVolume(1, to: track) }
        }
        volumeGains.removeAll()
        refreshParticipants()
    }

    private func applyVolume(_ gain: Double, to track: RemoteAudioTrack) {
        volumeUpdates.enqueue(for: track) { track.volume = gain }
    }

    /// The room of the call in progress, from the moment its connect starts.
    @ObservationIgnored private var room: Room?
    /// The room holds its delegates weakly.
    @ObservationIgnored private var observer: RoomObserver?

    // MARK: - Devices

    private(set) var devices = CallDevices()
    /// What the person chose, kept on this device (web keeps it in local storage).
    @ObservationIgnored private var devicePreferences: CallDevicePreferences
    @ObservationIgnored private let defaults: UserDefaults
    private static let devicePreferencesKey = "callDevicePreferences"
    private static let audioProcessingKey = "callAudioProcessing"
    private static let backgroundKey = "callBackground"
    private(set) var background = CallBackground.off
    private(set) var isBackgroundFailing = false
    /// What stands between each of our cameras and what its picture goes
    /// to, by capturer. Kept here: a capturer holds its processor weakly.
    @ObservationIgnored private var backgroundProcessors: [ObjectIdentifier: CallBackgroundProcessor] = [:]
    /// The processors that say their blur is not working. Each speaks for
    /// itself, so one that is replaced takes its word with it.
    @ObservationIgnored private var failingBackgrounds: Set<ObjectIdentifier> = []
    /// A camera asked for with blur on never got as far as being
    /// published: there is no camera, and no processor left to say so.
    @ObservationIgnored private var cameraFailedToStartBlurred = false
    /// The processors of a room that has been forgotten and is still being
    /// taken down. Its cameras go on capturing until the SDK has stopped
    /// them, and a capturer whose processor has been let go sends its
    /// frames as they are: so they are held until the disconnect is done.
    @ObservationIgnored private var departingBackgroundProcessors: [ObjectIdentifier: [CallBackgroundProcessor]] = [:]
    private(set) var audioProcessing = CallAudioProcessing()

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        devicePreferences = CallDevicePreferences(stored: defaults.dictionary(forKey: Self.devicePreferencesKey))
        audioProcessing = CallAudioProcessing(stored: defaults.dictionary(forKey: Self.audioProcessingKey))
        background = CallBackground(stored: defaults.string(forKey: Self.backgroundKey))
        #if os(macOS)
        // A headset plugged in mid-call is offered at once, and one the person chose comes back into use.
        AudioManager.shared.onDeviceUpdate = { [weak self] _ in
            Task { @MainActor in await self?.refreshDevices() }
        }
        #endif
        // A camera plugged in or pulled out: the list follows, and so does a camera that is on.
        for name in [AVCaptureDevice.wasConnectedNotification, AVCaptureDevice.wasDisconnectedNotification] {
            deviceObservers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                Task { @MainActor in
                    await self?.refreshDevices()
                    await self?.applyCameraChoice()
                }
            })
        }
    }

    @ObservationIgnored private nonisolated(unsafe) var deviceObservers: [any NSObjectProtocol] = []

    deinit {
        deviceObservers.forEach(NotificationCenter.default.removeObserver)
    }

    func refreshDevices() async {
        let cameras = ((try? await CameraCapturer.captureDevices()) ?? []).filter(Self.isOffered).map {
            CallDevice(id: $0.uniqueID, name: $0.localizedName)
        }
        var found = CallDevices(cameras: CallDeviceList(devices: cameras, preferred: devicePreferences[.camera]))
        #if os(macOS)
        // The system's own choice is our "System default" row, not a device of its own.
        func listed(_ all: [AudioDevice]) -> [CallDevice] {
            all.filter { !$0.isDefault }.map { CallDevice(id: $0.deviceId, name: $0.name) }
        }
        let audio = AudioManager.shared
        found.microphones = CallDeviceList(devices: listed(audio.inputDevices), preferred: devicePreferences[.microphone])
        found.speakers = CallDeviceList(devices: listed(audio.outputDevices), preferred: devicePreferences[.speaker])
        // A chosen device that is gone gives way to the system's, and takes over again when it is back.
        let input = audio.inputDevices.first { $0.deviceId == found.microphones.chosen?.id } ?? audio.defaultInputDevice
        if audio.inputDevice.deviceId != input.deviceId { audio.inputDevice = input }
        let output = audio.outputDevices.first { $0.deviceId == found.speakers.chosen?.id } ?? audio.defaultOutputDevice
        if audio.outputDevice.deviceId != output.deviceId { audio.outputDevice = output }
        #endif
        if devices != found { devices = found }
    }

    func chooseDevice(_ kind: CallDeviceKind, id: String?) async {
        devicePreferences[kind] = id
        defaults.set(devicePreferences.stored, forKey: Self.devicePreferencesKey)
        await refreshDevices()
        if kind == .camera { await applyCameraChoice() }
    }

    // MARK: - Diagnostics

    /// The tracks asked to report statistics. Asking one again restarts its
    /// timer, so each is asked once.
    @ObservationIgnored private var measuredTracks: [ObjectIdentifier: Track] = [:]
    /// How many are looking: a Mac can have the settings open in two
    /// windows, and closing one must not blind the other.
    @ObservationIgnored private var videoWatchers = 0
    private var measuresVideo: Bool { videoWatchers > 0 }

    func setMeasuresVideo(_ on: Bool) async {
        videoWatchers = max(0, videoWatchers + (on ? 1 : -1))
        guard measuresVideo else {
            let tracks = Array(measuredTracks.values)
            measuredTracks = [:]
            for track in tracks { await track.set(reportStatistics: false) }
            return
        }
        await measureNewVideoTracks()
    }

    /// One video stream of the call and the track that carries it.
    private struct VideoStream {
        let key: String
        let label: String
        let source: CallVideoSource
        let direction: CallVideoSample.Direction
        let track: Track
    }

    /// Every video stream of the call now: ours first, then the others'.
    private var videoStreams: [VideoStream] {
        guard let room else { return [] }
        let own: String = room.localParticipant.identity?.stringValue ?? joinedIdentity ?? ""
        var streams: [VideoStream] = []
        for publication in room.localParticipant.localVideoTracks where !publication.isMuted {
            guard let track = publication.track else { continue }
            let source: CallVideoSource = publication.source == .screenShareVideo ? .screen : .camera
            streams.append(VideoStream(key: "\(own):\(source):sent", label: "You", source: source, direction: .sent, track: track))
        }
        for (identity, camera) in cameraTracks.sorted(by: { $0.key < $1.key }) where identity != own {
            guard let track = camera as? Track else { continue }
            streams.append(VideoStream(
                key: "\(identity):camera:received", label: Self.name(of: identity), source: .camera, direction: .received,
                track: track
            ))
        }
        for (identity, screen) in screenTracks.sorted(by: { $0.key < $1.key }) {
            guard let track = screen.track as? Track else { continue }
            streams.append(VideoStream(
                key: "\(identity):screen:received", label: Self.name(of: identity), source: .screen, direction: .received,
                track: track
            ))
        }
        return streams
    }

    private static func name(of identity: String) -> String {
        JID(parsing: identity)?.bare.localpart ?? identity
    }

    private func measureNewVideoTracks() async {
        let measuring = room
        for stream in videoStreams where measuredTracks[ObjectIdentifier(stream.track)] == nil {
            measuredTracks[ObjectIdentifier(stream.track)] = stream.track
            await stream.track.set(reportStatistics: true)
            // The settings closed, or the call ended, while that one was being
            // switched on: it goes off again and nothing more is switched on.
            guard measuresVideo, room === measuring else {
                measuredTracks[ObjectIdentifier(stream.track)] = nil
                await stream.track.set(reportStatistics: false)
                return
            }
        }
        // A track that is no longer a stream, as a camera turned off, stops
        // being measured: muting does not stop the SDK's timer, and a track
        // only dropped from here would go on polling for the rest of the call.
        let live = Set(videoStreams.map { ObjectIdentifier($0.track) })
        for (id, track) in measuredTracks where !live.contains(id) {
            measuredTracks[id] = nil
            await track.set(reportStatistics: false)
        }
    }

    func videoSamples() async -> [CallVideoSample] {
        guard measuresVideo else { return [] }
        // A camera turned on while someone is looking is measured from then on.
        await measureNewVideoTracks()
        guard measuresVideo else { return [] }
        return videoStreams.map(Self.sample(of:))
    }

    private static func sample(of stream: VideoStream) -> CallVideoSample {
        var sample = CallVideoSample(
            key: stream.key, label: stream.label, source: stream.source, direction: stream.direction, timestamp: 0
        )
        guard let statistics = stream.track.statistics else { return sample }
        let candidate: LocalIceCandidateStatistics? = statistics.localIceCandidate
        sample.candidateType = candidate?.candidateType?.rawValue
        // A relay's own protocol is its leg to the peer, nearly always UDP;
        // what tells a TCP fallback is how we reach the relay (as web `iceCandidatePath`).
        if candidate?.candidateType == .relay {
            sample.transport = candidate?.relayProtocol.map { $0 == .udp ? "udp" : "tcp" }
        } else {
            sample.transport = candidate?.protocol
        }
        switch stream.direction {
        case .sent: fill(&sample, sentBy: stream.track, statistics)
        case .received: fill(&sample, received: statistics)
        }
        return sample
    }

    /// The codec a stream is carried in: the one its own entry names. A
    /// report also lists the retransmission codec, which is "video/rtx" and
    /// must never be the answer; without a name to go by, it is the first that is not.
    private static func codec(_ id: String?, in statistics: TrackStatistics) -> String? {
        let named: String? = id.flatMap { id in statistics.codec.first { $0.id == id }?.mimeType }
        if let named { return named }
        return statistics.codec.compactMap(\.mimeType).first { type in
            type.hasPrefix("video/") && type.lowercased() != "video/rtx"
        }
    }

    private static func fill(_ sample: inout CallVideoSample, sentBy track: Track, _ statistics: TrackStatistics) {
        // Simulcast sends several sizes: the figures are the largest one's, the bytes all of them.
        let layers: [OutboundRtpStreamStatistics] = statistics.outboundRtpStream
        func area(_ layer: OutboundRtpStreamStatistics) -> UInt { (layer.frameWidth ?? 0) * (layer.frameHeight ?? 0) }
        let top: OutboundRtpStreamStatistics? = layers.max { area($0) < area($1) }
        sample.width = top?.frameWidth.map { Int($0) }
        sample.height = top?.frameHeight.map { Int($0) }
        sample.framesPerSecond = top?.framesPerSecond
        sample.timestamp = top?.timestamp ?? 0
        sample.codec = codec(top?.codecId, in: statistics)
        // A backup codec for those who cannot decode the first has a sender of its own.
        let backups: [OutboundRtpStreamStatistics] = track.simulcastStatistics.values.flatMap(\.outboundRtpStream)
        let sent: [UInt64] = (layers + backups).compactMap(\.bytesSent)
        sample.bytes = sent.isEmpty ? nil : sent.reduce(0, +)
        // The far end's word on what arrived, over every layer as the web
        // counts it: one layer's report alone jumps about.
        let reports: [RemoteInboundRtpStreamStatistics] = statistics.remoteInboundRtpStream
        let lost: Int64 = reports.compactMap(\.packetsLost).reduce(0, +)
        let packets: UInt64 = layers.compactMap(\.packetsSent).reduce(0, +)
        if !reports.isEmpty, packets > 0 { sample.lossFraction = Double(max(lost, 0)) / Double(packets) }
        sample.roundTrip = reports.compactMap(\.roundTripTime).max()
    }

    private static func fill(_ sample: inout CallVideoSample, received statistics: TrackStatistics) {
        let stream: InboundRtpStreamStatistics? = statistics.inboundRtpStream.first
        sample.width = stream?.frameWidth.map { Int($0) }
        sample.height = stream?.frameHeight.map { Int($0) }
        sample.framesPerSecond = stream?.framesPerSecond
        sample.timestamp = stream?.timestamp ?? 0
        sample.bytes = stream?.bytesReceived
        sample.codec = codec(stream?.codecId, in: statistics)
        if let lost = stream?.packetsLost, let received = stream?.packetsReceived {
            let missing: Int64 = max(lost, 0)
            let total: Int64 = Int64(received) + missing
            if total > 0 { sample.lossFraction = Double(missing) / Double(total) }
        }
        sample.roundTrip = statistics.iceCandidatePair.first?.currentRoundTripTime
    }

    // MARK: - Camera background

    func setBackground(_ background: CallBackground) {
        guard background != self.background else { return }
        self.background = background
        defaults.set(background.rawValue, forKey: Self.backgroundKey)
        for processor in backgroundProcessors.values { processor.setBlurring(background == .blur) }
        if background == .off {
            failingBackgrounds = []
            cameraFailedToStartBlurred = false
            refreshBackgroundFailing()
        }
    }

    func restartBackground() async {
        for processor in backgroundProcessors.values { processor.restart() }
        // A blur that gave no picture while the camera was starting leaves no
        // camera at all: the SDK gives up on a track it gets no frames from.
        // Checking again then means starting the camera again, if it is still wanted.
        guard let room, publications(of: .video, in: room).isEmpty, captureChoices[.video] == true else { return }
        await setSource(.video, enabled: true, in: room)
    }

    /// The processor a new camera is made with (`CallBackgroundProcessor`),
    /// so that no frame of it gets out before the blur is in place.
    private func makeBackgroundProcessor() -> CallBackgroundProcessor {
        // The processor's word is filed under the processor, which does not exist until this returns.
        let made = MadeProcessor()
        let processor = CallBackgroundProcessor(isBlurring: background == .blur) { [weak self] isFailing in
            Task { @MainActor in
                guard let self, let processor = made.processor else { return }
                self.backgroundFailureChanged(isFailing, for: processor)
            }
        }
        made.processor = processor
        return processor
    }

    /// Lets a processor's callback name the processor it belongs to.
    private final class MadeProcessor: @unchecked Sendable {
        weak var processor: CallBackgroundProcessor?
    }

    private func backgroundFailureChanged(_ isFailing: Bool, for processor: CallBackgroundProcessor) {
        // A processor that has been let go has nothing more to say about the call.
        guard backgroundProcessors.values.contains(where: { $0 === processor }) else { return }
        if isFailing, background == .blur {
            failingBackgrounds.insert(ObjectIdentifier(processor))
        } else {
            failingBackgrounds.remove(ObjectIdentifier(processor))
        }
        refreshBackgroundFailing()
    }

    /// Call when `room` has finished disconnecting: its cameras are stopped, and their processors can go.
    private func releaseBackgroundProcessors(of room: Room) {
        departingBackgroundProcessors[ObjectIdentifier(room)] = nil
    }

    /// Lets a capturer's processor go, and its word about the blur with it.
    private func discardBackgroundProcessor(of capturer: ObjectIdentifier) {
        if let processor = backgroundProcessors.removeValue(forKey: capturer) {
            failingBackgrounds.remove(ObjectIdentifier(processor))
        }
        refreshBackgroundFailing()
    }

    /// The one thing the stage is told: the blur is on and no picture is getting out.
    private func refreshBackgroundFailing() {
        let isFailing = room != nil && background == .blur && (!failingBackgrounds.isEmpty || cameraFailedToStartBlurred)
        guard isBackgroundFailing != isFailing else { return }
        isBackgroundFailing = isFailing
        // Said again each time it happens, as a refused device is.
        if isFailing { mediaRefusals[.background, default: 0] += 1 }
    }

    /// The safety net under `publishCamera`: a camera of ours that somehow
    /// has no processor is given one.
    ///
    /// A processor is not let go because its camera is missing from the
    /// publications: a full reconnect takes the publication away and
    /// publishes the same track again, capturing before it reports, and a
    /// capturer whose processor was let go meanwhile would send frames as
    /// they are. Processors go when their track is dropped on purpose, and with the call.
    private func giveBackgroundToCameras(in room: Room) {
        for publication in publications(of: .video, in: room) {
            guard let capturer = (publication.track as? LocalVideoTrack)?.capturer else { continue }
            let id = ObjectIdentifier(capturer)
            if capturer.processor == nil || backgroundProcessors[id] == nil {
                let processor = backgroundProcessors[id] ?? makeBackgroundProcessor()
                backgroundProcessors[id] = processor
                capturer.processor = processor
            }
        }
    }

    /// Unmutes the camera we have, or makes one. Every camera this
    /// controller ever sends from is one `publishCamera` made.
    private func turnCameraOn(in room: Room) async {
        for publication in publications(of: .video, in: room) where publication.isMuted {
            try? await publication.unmute()
        }
        // None to unmute, or it went away while we were at it, as a muted
        // track does in a full reconnect: a new one, made our way.
        if self.room === room, !publications(of: .video, in: room).contains(where: { !$0.isMuted }) {
            await publishCamera(in: room)
        }
    }

    /// Makes and publishes a camera of our own making. The SDK's own way
    /// makes the track without a processor and sends frames before one can
    /// be attached; ours has it from before the first frame.
    private func publishCamera(in room: Room) async {
        let processor = makeBackgroundProcessor()
        let track = await LocalVideoTrack.createCameraTrack(options: cameraCaptureOptions(), processor: processor)
        let id = ObjectIdentifier(track.capturer)
        guard self.room === room else {
            // The call ended while the camera was being made. `setSource`
            // takes the room down next; the processor is held until then
            // and never counted among those of a later call.
            departingBackgroundProcessors[ObjectIdentifier(room), default: []].append(processor)
            _ = try? await track.stop()
            return
        }
        backgroundProcessors[id] = processor
        // The choice may have changed while the track was being made, when
        // this processor was not yet among those `setBackground` reaches.
        processor.setBlurring(background == .blur)
        guard (try? await room.localParticipant.publish(videoTrack: track)) != nil else {
            // Not published, as when the blur gave it no frame to start
            // with: nothing of it is left running, and "Check again" starts
            // another. Its processor goes, so the failure is kept here.
            discardBackgroundProcessor(of: id)
            _ = try? await track.stop()
            if self.room === room, background == .blur {
                cameraFailedToStartBlurred = true
                refreshBackgroundFailing()
            }
            return
        }
        // A camera that came up puts an earlier failed start behind us.
        if cameraFailedToStartBlurred {
            cameraFailedToStartBlurred = false
            refreshBackgroundFailing()
        }
    }

    // MARK: - Audio processing

    func setAudioProcessing(_ processing: CallAudioProcessing) async {
        guard processing != audioProcessing else { return }
        audioProcessing = processing
        defaults.set(processing.stored, forKey: Self.audioProcessingKey)
        await applyAudioProcessing()
    }

    private var audioCaptureOptions: AudioCaptureOptions {
        AudioCaptureOptions(
            echoCancellation: audioProcessing.echoCancellation, autoGainControl: audioProcessing.autoGainControl,
            noiseSuppression: audioProcessing.noiseSuppression
        )
    }

    /// Gives the processing chosen to the microphone's track as it is, on
    /// or muted: the SDK changes it in place, with no new track and no gap
    /// in what others hear. A track keeps the options it was made with for
    /// its own restarts, so this runs again after a reconnect.
    private func applyAudioProcessing() async {
        guard let room else { return }
        let wanted = audioProcessing
        let options = AudioProcessingOptions(
            echoCancellation: wanted.echoCancellation, autoGainControl: wanted.autoGainControl,
            noiseSuppression: wanted.noiseSuppression
        )
        let published = publications(of: .audio, in: room)
        let tracks = published.compactMap { $0.track as? LocalAudioTrack }
        let publicationIDs = Set(published.map(\.sid.stringValue))
        guard !tracks.isEmpty else { return }
        // The SDK waits on WebRTC's signalling thread for each write, so
        // they leave the main thread; one queue keeps them in the order asked.
        let accepted = await withCheckedContinuation { (done: CheckedContinuation<Bool, Never>) in
            Self.audioProcessingWrites.async {
                let results = tracks.map { (try? $0.setAudioProcessingOptions(options)) != nil }
                done.resume(returning: !results.contains(false))
            }
        }
        // The latest choice may have moved on while this one was written; its own write follows.
        if accepted, wanted == audioProcessing {
            appliedAudioProcessing = wanted
            audioProcessingPublications = publicationIDs
        }
    }

    /// The microphone publications that have the choice. A reconnect takes
    /// the publication away and publishes the track again, restarted with
    /// the options it was made with, so one that is not here has yet to be given it.
    @ObservationIgnored private var audioProcessingPublications: Set<String> = []

    /// Gives the choice to a microphone publication that has not had it: one
    /// that came back after a reconnect, or appeared while a change found none.
    private func giveAudioProcessingToNewPublications(in room: Room) {
        let current = Set(publications(of: .audio, in: room).map(\.sid.stringValue))
        guard !current.isEmpty, current != audioProcessingPublications || appliedAudioProcessing != audioProcessing else { return }
        Task { await applyAudioProcessing() }
    }

    private static let audioProcessingWrites = DispatchQueue(label: "waddle.call-audio-processing", qos: .userInitiated)
    /// The processing the microphone's track last took, for tests: `nil` until one has.
    @ObservationIgnored private(set) var appliedAudioProcessing: CallAudioProcessing?
    /// The microphone's publication, for tests: a change of processing must not replace it.
    var microphonePublicationID: String? {
        room.flatMap { publications(of: .audio, in: $0).first?.sid.stringValue }
    }

    /// Moves a camera that is on to the one that should be in use. A camera
    /// that is off is never touched: changing a capturer's options restarts
    /// its capture, which would turn the camera on behind a control that
    /// says off. Its track is dropped, and the next turn-on makes a new one
    /// with the choice.
    ///
    /// - Parameter droppingIdleTracks: off for the check made as a camera
    ///   finishes turning on, which is about the camera that is now on.
    private func applyCameraChoice(droppingIdleTracks: Bool = true) async {
        // One at a time. A change stops the capture and starts it again, and
        // the SDK counts those: two changes at once can leave the count a
        // start ahead, and then turning the camera off no longer stops it.
        let earlier = cameraChoiceWork
        let work = Task { [weak self] in
            await earlier?.value
            await self?.moveCameraToChoice(droppingIdleTracks: droppingIdleTracks)
        }
        cameraChoiceWork = work
        await work.value
    }

    /// The latest change to the camera in use; the next waits for it.
    @ObservationIgnored private var cameraChoiceWork: Task<Void, Never>?

    private func moveCameraToChoice(droppingIdleTracks: Bool) async {
        guard let room else { return }
        let wanted = cameraCaptureOptions()
        for publication in publications(of: .video, in: room) {
            if publication.isMuted {
                if droppingIdleTracks {
                    if let capturer = (publication.track as? LocalVideoTrack)?.capturer {
                        discardBackgroundProcessor(of: ObjectIdentifier(capturer))
                    }
                    try? await room.localParticipant.unpublish(publication: publication)
                }
            } else if let capturer = (publication.track as? LocalVideoTrack)?.capturer as? CameraCapturer,
                      capturer.options.device?.uniqueID != wanted?.device?.uniqueID {
                _ = try? await capturer.set(options: wanted ?? CameraCaptureOptions())
                // The change stops the capture and starts it again. A camera
                // turned off, or a call left, between the two finds nothing
                // to stop, and the restart would then run on behind it.
                if self.room !== room || publication.isMuted || captureChoices[.video] == false {
                    _ = try? await capturer.stopCapture()
                    if self.room === room, !publication.isMuted { try? await publication.mute() }
                }
            }
        }
        if self.room === room { refreshTracks() }
    }

    /// On iPhone and iPad the system lists every lens, and the combinations
    /// of them, as cameras of their own: one front and one back is the choice a person has.
    private static func isOffered(_ camera: AVCaptureDevice) -> Bool {
        #if os(iOS)
        camera.deviceType == .builtInWideAngleCamera || camera.deviceType == .external
        #else
        true
        #endif
    }

    /// The camera the person chose, while it is connected; `nil` leaves the choice to the system.
    private func cameraCaptureOptions() -> CameraCaptureOptions? {
        (devices.cameras.chosen?.id).flatMap(AVCaptureDevice.init(uniqueID:)).map { CameraCaptureOptions(device: $0) }
    }

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
            releaseBackgroundProcessors(of: room)
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
        releaseBackgroundProcessors(of: room)
    }

    func forgetCaptureChoices() {
        captureChoices = [:]
        if isMicrophoneMutedByChoice { isMicrophoneMutedByChoice = false }
        volumeUpdates.removeAll()
        resetVolumes()
    }

    func setMicrophone(_ on: Bool) async {
        captureChoices[.audio] = on
        if isMicrophoneMutedByChoice == on { isMicrophoneMutedByChoice = !on }
        guard let room else { return }
        await setSource(.audio, enabled: on, in: room)
    }

    func setCamera(_ on: Bool) async {
        captureChoices[.video] = on
        if !on, cameraFailedToStartBlurred {
            // A camera they no longer want is not a camera that failed to start.
            cameraFailedToStartBlurred = false
            refreshBackgroundFailing()
        }
        guard let room else { return }
        await setSource(.video, enabled: on, in: room)
    }

    // MARK: - Sharing our screen

    #if os(macOS)
    var canShareScreen: Bool { true }

    func screenSources() async -> CallScreenSources? {
        if didScreenShareFail { didScreenShareFail = false }
        // The first request shows the system's prompt and returns at once; the
        // listing below then fails until the person has allowed it.
        if !CGPreflightScreenCaptureAccess() { CGRequestScreenCaptureAccess() }
        guard let found = await capturableContent() else {
            // Refused, or not yet answered: either way there is nothing to pick from.
            isScreenShareBlocked = !CGPreflightScreenCaptureAccess()
            if isScreenShareBlocked { mediaRefusals[.screen, default: 0] += 1 }
            return isScreenShareBlocked ? nil : CallScreenSources(CallScreenCandidates(), ownBundleID: nil)
        }
        if isScreenShareBlocked { isScreenShareBlocked = false }
        return CallScreenSources(found.candidates, ownBundleID: Bundle.main.bundleIdentifier)
    }

    func shareScreen(_ source: CallScreenSource) async {
        // One at a time, in the order asked: a second pick waits for the
        // first to be published and then replaces it.
        screenShareGeneration += 1
        let generation = screenShareGeneration
        let earlier = screenShareWork
        let work = Task { [weak self] in
            await earlier?.value
            await self?.startScreenShare(source, generation: generation)
        }
        screenShareWork = work
        await work.value
    }

    func stopScreenShare() async {
        // Overtakes a share that is still starting: it is taken down as soon as it is up.
        wantsScreenShare = false
        screenShareGeneration += 1
        let earlier = screenShareWork
        let work = Task { [weak self] in
            await earlier?.value
            await self?.unpublishScreens()
        }
        screenShareWork = work
        await work.value
    }

    private func startScreenShare(_ source: CallScreenSource, generation: Int) async {
        guard generation == screenShareGeneration, let room else { return }
        await unpublishScreens()
        guard generation == screenShareGeneration else { return }
        if didScreenShareFail { didScreenShareFail = false }
        wantsScreenShare = true
        isScreenShareStarting = true
        defer { isScreenShareStarting = false }
        // Listed again now: the picker's list may be minutes old, and a
        // screen is captured with the applications that exist when it is listed.
        guard room.connectionState == .connected, let capture = await capturableContent()?.sources[source.id] else {
            if self.room === room, generation == screenShareGeneration {
                wantsScreenShare = false
                didScreenShareFail = true
                mediaRefusals[.screen, default: 0] += 1
            }
            return
        }
        let track = await LocalVideoTrack.createMacOSScreenShareTrack(
            source: capture,
            options: ScreenShareCaptureOptions(dimensions: .h1440_169, fps: Self.screenShareFPS, showCursor: true)
        )
        // As the web publishes a shared screen: VP9 with VP8 for those who
        // cannot decode it, up to 5 Mbps so dense text stays sharp, and
        // resolution kept over frame rate when the network is short.
        let options = VideoPublishOptions(
            screenShareEncoding: VideoEncoding(maxBitrate: 5_000_000, maxFps: Self.screenShareFPS),
            preferredCodec: .vp9, preferredBackupCodec: .vp8, degradationPreference: .maintainResolution
        )
        // The system, or the window closing, can end a capture without a word
        // to the room. Watched from before the publish, which starts the
        // capture: one that ends while it is still being published stops the share too.
        // It speaks for this capture only: one we take down ourselves, for a
        // stop or a newer pick, has been overtaken and must not stop what came after it.
        let watcher = ScreenCaptureWatcher { [weak self] in
            Task { @MainActor in
                guard let self, self.screenShareGeneration == generation else { return }
                await self.stopScreenShare()
            }
        }
        track.capturer.delegates.add(delegate: watcher)
        screenCaptureWatcher = watcher
        let publication = try? await room.localParticipant.publish(videoTrack: track, options: options)
        guard self.room === room, generation == screenShareGeneration else {
            // The call ended, or the person or the system stopped, while it
            // was starting: the screen must not go on being captured for nobody.
            track.capturer.delegates.remove(delegate: watcher)
            if screenCaptureWatcher === watcher { screenCaptureWatcher = nil }
            if let publication { try? await room.localParticipant.unpublish(publication: publication) }
            _ = try? await track.stop()
            return
        }
        // Published, and still being captured: a capture can end before its watcher hears of it.
        guard let publication, track.capturer.captureState == .started else {
            track.capturer.delegates.remove(delegate: watcher)
            if let publication { try? await room.localParticipant.unpublish(publication: publication) }
            _ = try? await track.stop()
            screenCaptureWatcher = nil
            wantsScreenShare = false
            didScreenShareFail = true
            mediaRefusals[.screen, default: 0] += 1
            refreshTracks()
            return
        }
        refreshTracks()
    }

    private func unpublishScreens() async {
        screenCaptureWatcher = nil
        guard let room else { return }
        for publication in room.localParticipant.localVideoTracks where publication.source == .screenShareVideo {
            try? await room.localParticipant.unpublish(publication: publication)
        }
        if self.room === room { refreshTracks() }
    }

    /// What the system will let us capture now, or `nil` when it will not say.
    private func capturableContent() async -> (candidates: CallScreenCandidates, sources: [String: any MacOSScreenCaptureSource])? {
        guard let displays = try? await MacOSScreenCapturer.displaySources(),
              let windows = try? await MacOSScreenCapturer.windowSources() else { return nil }
        let main = CGMainDisplayID()
        var sources: [String: any MacOSScreenCaptureSource] = [:]
        var candidates = CallScreenCandidates()
        for display in displays {
            sources["screen:\(display.displayID)"] = display
            candidates.screens.append(.init(
                number: display.displayID, width: display.width, height: display.height, isMain: display.displayID == main
            ))
        }
        for window in windows {
            sources["window:\(window.windowID)"] = window
            candidates.windows.append(.init(
                number: window.windowID, title: window.title, application: window.owningApplication?.applicationName,
                bundleID: window.owningApplication?.bundleIdentifier, layer: window.windowLayer,
                isOnScreen: window.isOnScreen, size: window.frame.size
            ))
        }
        return (candidates, sources)
    }

    private static let screenShareFPS = 30
    #else
    var canShareScreen: Bool { false }

    func screenSources() async -> CallScreenSources? { nil }

    func shareScreen(_: CallScreenSource) async { /* Sharing from iOS needs a broadcast extension; not built. */ }

    func stopScreenShare() async { /* Nothing is ever shared from here. */ }
    #endif

    /// Starts what the call agreed to, unless the person has since chosen
    /// otherwise for that device.
    private func publish(_ media: CallMedia, in room: Room) async {
        // Here and not before the room exists: a hang-up must find a room to
        // disconnect from the moment its connect starts.
        await refreshDevices()
        guard self.room === room else { return }
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
            _ = try? await room.localParticipant.setMicrophone(enabled: enabled, captureOptions: enabled ? audioCaptureOptions : nil)
        } else {
            // The SDK's own `setCamera` is never asked to turn a camera on:
            // it looks for the publication again inside its own queue and,
            // finding none, as during a reconnect, makes a camera with no
            // background processor, whose frames go out as they are. A
            // camera of ours is unmuted here, and one that is not there is
            // made by `publishCamera`, with its processor on it from the start.
            if enabled { await turnCameraOn(in: room) }
        }
        // The SDK toggles one publication per device. Off means all of them.
        // And a device turned off while it was still starting is off: the
        // choice as it stands now, not as it stood when the start was asked
        // for, as when Space held to talk came up before the microphone had opened.
        if !enabled || captureChoices[kind] == false {
            for publication in publications(of: kind, in: room) where !publication.isMuted {
                try? await publication.mute()
            }
        }
        // The call ended while a track was starting: `disconnect()` ran
        // before the track was published, so release it here.
        guard self.room === room else {
            await room.localParticipant.unpublishAll()
            await room.disconnect()
            releaseBackgroundProcessors(of: room)
            return
        }
        refreshTracks()
        // The camera was started with the choice as it was when it was asked
        // for; a choice made while it was starting applies now that it is on.
        if kind == .video, enabled { await applyCameraChoice(droppingIdleTracks: false) }
        // Likewise a change to the processing made while the microphone was starting.
        if kind == .audio, enabled { await applyAudioProcessing() }
    }

    /// Asks for the permission, but only for a call that still owns `room`,
    /// and checks again once the user has answered: a prompt can stay open
    /// long after a hang-up. An ended call neither prompts nor captures.
    private func mayCapture(_ kind: AVMediaType, in room: Room) async -> Bool {
        guard self.room === room else { return false }
        let isAllowed = await AVCaptureDevice.requestAccess(for: kind)
        // Kept for the dock, which says why the control does nothing.
        if kind == .audio { isMicrophoneBlocked = !isAllowed } else { isCameraBlocked = !isAllowed }
        if !isAllowed { mediaRefusals[kind == .audio ? .microphone : .camera, default: 0] += 1 }
        return isAllowed && self.room === room
    }

    private func forgetRoom() {
        let departing = room
        room = nil
        observer = nil
        joinedIdentity = nil
        volumeUpdates.removeAll()
        phase = .idle
        connectedSince = nil
        mediaUpSince = nil
        // What a call could not use was that call's: the next has not been
        // asked for a camera yet, and may be allowed one by now.
        if isMicrophoneBlocked { isMicrophoneBlocked = false }
        if isCameraBlocked { isCameraBlocked = false }
        if !mediaRefusals.isEmpty { mediaRefusals = [:] }
        // What was said about sharing the screen was said about that call.
        if isScreenShareBlocked { isScreenShareBlocked = false }
        if didScreenShareFail { didScreenShareFail = false }
        connectionSettling?.cancel()
        connectionRating = CallConnectionRatingGate()
        appliedAudioProcessing = nil
        // Forgotten at once, so nothing stale is heard; let go only when the room is down.
        if let departing, !backgroundProcessors.isEmpty {
            departingBackgroundProcessors[ObjectIdentifier(departing), default: []] += Array(backgroundProcessors.values)
        }
        backgroundProcessors = [:]
        failingBackgrounds = []
        cameraFailedToStartBlurred = false
        if isBackgroundFailing { isBackgroundFailing = false }
        measuredTracks = [:]
        videoWatchers = 0
        audioProcessingPublications = []
        #if os(macOS)
        // A share still starting finds its call gone and lets the screen go.
        wantsScreenShare = false
        screenShareGeneration += 1
        screenCaptureWatcher = nil
        #endif
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
                guard !Task.isCancelled, let self, self.room === changed else { return }
                self.connectionRating.settled()
                self.refreshParticipants()
            }
            // A reconnect restarts the microphone's track with the options it was made with.
            Task { await applyAudioProcessing() }
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
        giveBackgroundToCameras(in: room)
        giveAudioProcessingToNewPublications(in: room)
        #if os(macOS)
        // A full reconnect republishes what it had: not a screen the person has stopped sharing since.
        if !wantsScreenShare, room.localParticipant.localVideoTracks.contains(where: { $0.source == .screenShareVideo }) {
            Task { await unpublishScreens() }
        }
        #endif
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
                isSpeaking: participant.isSpeaking, volume: isLocal ? 1 : volumeGains[identity] ?? 1
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

#if os(macOS)
/// Tells when the capture of our shared screen stops by itself.
private final class ScreenCaptureWatcher: NSObject, VideoCapturerDelegate, @unchecked Sendable {
    private let stopped: @Sendable () -> Void

    init(stopped: @escaping @Sendable () -> Void) {
        self.stopped = stopped
    }

    func capturer(_: VideoCapturer, didUpdate state: VideoCapturer.CapturerState) {
        if state == .stopped { stopped() }
    }
}
#endif

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
    /// Someone joined, or the SDK reported who is speaking and how loudly.
    case participants
    /// The server rated our own connection.
    case ownConnectionRated
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

    func room(_ room: Room, participant: Participant, didUpdateConnectionQuality _: ConnectionQuality) {
        // Only ours is shown.
        if participant is LocalParticipant { forward(.ownConnectionRated, from: room) }
    }
}
