import AVFoundation
import Foundation
import LiveKit
import Observation
import WaddleKit

/// Owns our camera's background and applies the selected camera device.
///
/// Every camera we make goes through `publishCamera`, with its
/// `CallBackgroundProcessor` attached before the first frame and synced
/// to the latest background choice before publish. Processors stay alive
/// across a reconnect's republication and in `departingBackgroundProcessors`
/// until the room's disconnect has finished. The SDK's own
/// `setCamera(enabled: true)` is never called.
@MainActor
@Observable
final class CallCamera {
    @ObservationIgnored private let roomAccess: CallRoomAccess
    @ObservationIgnored private let capture: any CallCaptureSources
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let chosenCameraID: @MainActor () -> String?
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

    init(
        roomAccess: CallRoomAccess, capture: any CallCaptureSources, defaults: UserDefaults,
        chosenCameraID: @escaping @MainActor () -> String?
    ) {
        self.roomAccess = roomAccess
        self.capture = capture
        self.defaults = defaults
        self.chosenCameraID = chosenCameraID
        background = CallBackground(stored: defaults.string(forKey: Self.backgroundKey))
    }

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

    func restartBackground(
        wantsCamera: @MainActor () -> Bool, startCamera: @MainActor (Room) async -> Void
    ) async {
        for processor in backgroundProcessors.values { processor.restart() }
        // A blur that gave no picture while the camera was starting leaves no
        // camera at all: the SDK gives up on a track it gets no frames from.
        // Checking again then means starting the camera again, if it is still wanted.
        guard let room = roomAccess.room, cameraPublications(in: room).isEmpty, wantsCamera() else { return }
        await startCamera(room)
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
    func releaseBackgroundProcessors(of room: Room) {
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
        let isFailing = roomAccess.room != nil && background == .blur && (!failingBackgrounds.isEmpty || cameraFailedToStartBlurred)
        guard isBackgroundFailing != isFailing else { return }
        isBackgroundFailing = isFailing
        // Said again each time it happens, as a refused device is.
        if isFailing { roomAccess.recordRefusal(.background) }
    }

    /// The safety net under `publishCamera`: a camera of ours that somehow
    /// has no processor is given one.
    ///
    /// A processor is not let go because its camera is missing from the
    /// publications: a full reconnect takes the publication away and
    /// publishes the same track again, capturing before it reports, and a
    /// capturer whose processor was let go meanwhile would send frames as
    /// they are. Processors go when their track is dropped on purpose, and with the call.
    func giveBackgroundToCameras(in room: Room) {
        for publication in cameraPublications(in: room) {
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
    /// call ever sends from is one `publishCamera` made.
    ///
    /// The SDK's own `setCamera` is never asked to turn a camera on:
    /// it looks for the publication again inside its own queue and,
    /// finding none, as during a reconnect, makes a camera with no
    /// background processor, whose frames go out as they are. A
    /// camera of ours is unmuted here, and one that is not there is
    /// made by `publishCamera`, with its processor on it from the start.
    func turnCameraOn(in room: Room) async {
        for publication in cameraPublications(in: room) where publication.isMuted {
            try? await publication.unmute()
        }
        // None to unmute, or it went away while we were at it, as a muted
        // track does in a full reconnect: a new one, made our way.
        if roomAccess.holds(room), !cameraPublications(in: room).contains(where: { !$0.isMuted }) {
            await publishCamera(in: room)
        }
    }

    /// Makes and publishes a camera of our own making. The SDK's own way
    /// makes the track without a processor and sends frames before one can
    /// be attached; ours has it from before the first frame.
    private func publishCamera(in room: Room) async {
        let processor = makeBackgroundProcessor()
        let track = await capture.makeCamera(options: cameraCaptureOptions(), processor: processor)
        let id = ObjectIdentifier(track.capturer)
        guard roomAccess.holds(room) else {
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
            if roomAccess.holds(room), background == .blur {
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

    func cameraNoLongerWanted() {
        // A camera they no longer want is not a camera that failed to start.
        guard cameraFailedToStartBlurred else { return }
        cameraFailedToStartBlurred = false
        refreshBackgroundFailing()
    }

    func roomWasForgotten(_ departing: Room?) {
        // Forgotten at once, so nothing stale is heard; let go only when the room is down.
        if let departing, !backgroundProcessors.isEmpty {
            departingBackgroundProcessors[ObjectIdentifier(departing), default: []] += Array(backgroundProcessors.values)
        }
        backgroundProcessors = [:]
        failingBackgrounds = []
        cameraFailedToStartBlurred = false
        if isBackgroundFailing { isBackgroundFailing = false }
    }

    /// Moves a camera that is on to the one that should be in use. A camera
    /// that is off is never touched: changing a capturer's options restarts
    /// its capture, which would turn the camera on behind a control that
    /// says off. Its track is dropped, and the next turn-on makes a new one
    /// with the choice.
    ///
    /// - Parameter droppingIdleTracks: off for the check made as a camera
    ///   finishes turning on, which is about the camera that is now on.
    func applyCameraChoice(
        droppingIdleTracks: Bool = true, cameraChoice: @escaping @MainActor () -> Bool?
    ) async {
        // One at a time. A change stops the capture and starts it again, and
        // the SDK counts those: two changes at once can leave the count a
        // start ahead, and then turning the camera off no longer stops it.
        let earlier = cameraChoiceWork
        let work = Task { [weak self] in
            await earlier?.value
            await self?.moveCameraToChoice(droppingIdleTracks: droppingIdleTracks, cameraChoice: cameraChoice)
        }
        cameraChoiceWork = work
        await work.value
    }

    /// The latest change to the camera in use; the next waits for it.
    @ObservationIgnored private var cameraChoiceWork: Task<Void, Never>?

    private func moveCameraToChoice(droppingIdleTracks: Bool, cameraChoice: @MainActor () -> Bool?) async {
        guard let room = roomAccess.room else { return }
        let wanted = cameraCaptureOptions()
        for publication in cameraPublications(in: room) {
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
                if !roomAccess.holds(room) || publication.isMuted || cameraChoice() == false {
                    _ = try? await capturer.stopCapture()
                    if roomAccess.holds(room), !publication.isMuted { try? await publication.mute() }
                }
            }
        }
        if roomAccess.holds(room) { roomAccess.tracksChanged() }
    }

    /// The camera the person chose, while it is connected; `nil` leaves the choice to the system.
    private func cameraCaptureOptions() -> CameraCaptureOptions? {
        chosenCameraID().flatMap(AVCaptureDevice.init(uniqueID:)).map { CameraCaptureOptions(device: $0) }
    }

    /// Every publication of our camera. There is normally one.
    private func cameraPublications(in room: Room) -> [LocalTrackPublication] {
        room.localParticipant.localVideoTracks.filter { $0.source == .camera }
    }
}
