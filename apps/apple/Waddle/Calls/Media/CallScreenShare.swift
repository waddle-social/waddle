import Foundation
import LiveKit
import Observation
import WaddleKit

@MainActor
@Observable
final class CallScreenShare {
    @ObservationIgnored private let roomAccess: CallRoomAccess
    @ObservationIgnored private let capture: any CallCaptureSources

    init(roomAccess: CallRoomAccess, capture: any CallCaptureSources) {
        self.roomAccess = roomAccess
        self.capture = capture
    }

    private(set) var isScreenShareBlocked = false
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

    func resetRefusals() {
        // What was said about sharing the screen was said about that call.
        if isScreenShareBlocked { isScreenShareBlocked = false }
        if didScreenShareFail { didScreenShareFail = false }
    }

    func roomDidGoAway() {
        #if os(macOS)
        // A share still starting finds its call gone and lets the screen go.
        wantsScreenShare = false
        screenShareGeneration += 1
        screenCaptureWatcher = nil
        #endif
    }

    func tracksDidChange(in room: Room) {
        #if os(macOS)
        // A full reconnect republishes what it had: not a screen the person has stopped sharing since.
        if !wantsScreenShare, room.localParticipant.localVideoTracks.contains(where: { $0.source == .screenShareVideo }) {
            Task { await unpublishScreens() }
        }
        #endif
    }

    #if os(macOS)
    var canShareScreen: Bool { true }

    func screenSources() async -> CallScreenSources? {
        if didScreenShareFail { didScreenShareFail = false }
        // The first request shows the system's prompt and returns at once; the
        // listing below then fails until the person has allowed it.
        if !capture.isScreenCaptureAllowed { capture.requestScreenCapture() }
        guard let found = await capture.screenContent() else {
            // Refused, or not yet answered: either way there is nothing to pick from.
            isScreenShareBlocked = !capture.isScreenCaptureAllowed
            if isScreenShareBlocked { roomAccess.recordRefusal(.screen) }
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
        guard generation == screenShareGeneration, let room = roomAccess.room else { return }
        await unpublishScreens()
        guard generation == screenShareGeneration else { return }
        if didScreenShareFail { didScreenShareFail = false }
        wantsScreenShare = true
        isScreenShareStarting = true
        defer { isScreenShareStarting = false }
        // Listed again now: the picker's list may be minutes old, and a
        // screen is captured with the applications that exist when it is listed.
        guard room.connectionState == .connected, let makeTrack = await capture.screenContent()?.makeTrack[source.id] else {
            if roomAccess.holds(room), generation == screenShareGeneration {
                wantsScreenShare = false
                didScreenShareFail = true
                roomAccess.recordRefusal(.screen)
            }
            return
        }
        let track = await makeTrack(ScreenShareCaptureOptions(dimensions: .h1440_169, fps: Self.screenShareFPS, showCursor: true))
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
        guard roomAccess.holds(room), generation == screenShareGeneration else {
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
            // Said before the cleanup can suspend: by then the call may have
            // ended or a newer pick begun, and this would be said about that.
            track.capturer.delegates.remove(delegate: watcher)
            if screenCaptureWatcher === watcher { screenCaptureWatcher = nil }
            wantsScreenShare = false
            didScreenShareFail = true
            roomAccess.recordRefusal(.screen)
            if let publication { try? await room.localParticipant.unpublish(publication: publication) }
            _ = try? await track.stop()
            if roomAccess.holds(room) { roomAccess.tracksChanged() }
            return
        }
        roomAccess.tracksChanged()
    }

    private func unpublishScreens() async {
        screenCaptureWatcher = nil
        guard let room = roomAccess.room else { return }
        for publication in room.localParticipant.localVideoTracks where publication.source == .screenShareVideo {
            try? await room.localParticipant.unpublish(publication: publication)
        }
        if roomAccess.holds(room) { roomAccess.tracksChanged() }
    }

    private static let screenShareFPS = 30
    #else
    var canShareScreen: Bool { false }

    func screenSources() async -> CallScreenSources? { nil }

    func shareScreen(_: CallScreenSource) async { /* Sharing from iOS needs a broadcast extension; not built. */ }

    func stopScreenShare() async { /* Nothing is ever shared from here. */ }
    #endif
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
