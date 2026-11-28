import AVFoundation
import LiveKit
import Observation
import WaddleKit

/// Starts the devices a call wants and reports why a start did not work.
@MainActor
@Observable
final class CallDeviceCapture {
    private(set) var issues: [CallMediaIssue.Kind: CallMediaIssue.Reason] = [:]

    @ObservationIgnored private let roomAccess: CallRoomAccess
    @ObservationIgnored private let sources: any CallCaptureSources
    @ObservationIgnored private let catalog: CallDeviceCatalog
    @ObservationIgnored private let camera: CallCamera
    @ObservationIgnored private let microphone: CallMicrophoneProcessing
    @ObservationIgnored private var changing: Set<AVMediaType> = []

    init(
        roomAccess: CallRoomAccess, sources: any CallCaptureSources, catalog: CallDeviceCatalog,
        camera: CallCamera, microphone: CallMicrophoneProcessing
    ) {
        self.roomAccess = roomAccess
        self.sources = sources
        self.catalog = catalog
        self.camera = camera
        self.microphone = microphone
    }

    func setSource(_ kind: AVMediaType, enabled: Bool, in room: Room, wantsCapture: @escaping @MainActor () -> Bool) async {
        guard roomAccess.holds(room) else { return }
        let issueKind: CallMediaIssue.Kind = kind == .audio ? .microphone : .camera
        if !enabled { clearIssue(issueKind) }
        // One operation per device. If a toggle arrives while the operation
        // waits for permission or capture, its latest choice is applied below.
        guard changing.insert(kind).inserted else { return }

        var turnOn = wantsCapture()
        var started = false
        while roomAccess.holds(room) {
            if turnOn {
                let outcome = await CallCaptureAttempt.run(
                    isCurrent: { self.roomAccess.holds(room) && wantsCapture() },
                    requestAccess: { await self.sources.requestAccess(for: kind) },
                    deviceAvailability: { await self.catalog.captureAvailability(for: kind) },
                    start: { await self.start(kind, in: room, wantsCapture: wantsCapture) }
                )
                if roomAccess.holds(room), wantsCapture() {
                    // It was turned off and back on just as the earlier
                    // attempt returned. The latest choice still needs a start.
                    if outcome == .superseded { continue }
                    record(outcome, for: issueKind)
                }
                started = outcome == .started
            } else {
                await mute(kind, in: room)
                started = false
            }
            if wantsCapture() == turnOn { break }
            turnOn.toggle()
        }
        guard roomAccess.holds(room) else { return }
        changing.remove(kind)
        roomAccess.tracksChanged()

        // These settings can wait behind other SDK work. Release the
        // device first so a mute or push-to-talk release can go ahead.
        guard started, wantsCapture() else { return }
        if kind == .video {
            await camera.applyCameraChoice(droppingIdleTracks: false) { wantsCapture() }
        } else {
            await microphone.applyAudioProcessing()
        }
    }

    func roomDidClose() {
        if !issues.isEmpty { issues = [:] }
        changing = []
    }

    func clearIssue(_ kind: CallMediaIssue.Kind) {
        if issues[kind] != nil { issues[kind] = nil }
    }

    private func record(_ outcome: CallCaptureAttempt.Outcome, for kind: CallMediaIssue.Kind) {
        switch outcome {
        case .started: clearIssue(kind)
        case .failed(let reason):
            issues[kind] = reason
            roomAccess.recordRefusal(kind)
        case .superseded: break
        }
    }

    private func start(
        _ kind: AVMediaType, in room: Room, wantsCapture: @escaping @MainActor () -> Bool
    ) async -> CallMediaIssue.Reason? {
        let issue: CallMediaIssue.Reason?
        if kind == .audio {
            #if os(macOS)
            let microphoneID = catalog.devices.microphones.chosen?.id
            #endif
            do {
                let publication = try await room.localParticipant.setMicrophone(
                    enabled: true, captureOptions: microphone.audioCaptureOptions
                )
                issue = publication == nil ? .failed : nil
            } catch {
                #if os(macOS)
                if (error as? LiveKitError)?.type == .audioEngine, CallMicrophoneUse.isHeldByAnotherProcess(microphoneID) {
                    issue = .inUse
                } else {
                    issue = CallDeviceStartError.reason(for: error)
                }
                #else
                issue = CallDeviceStartError.reason(for: error)
                #endif
            }
        } else {
            issue = await camera.turnCameraOn(in: room) { self.roomAccess.holds(room) && wantsCapture() }
        }

        // The call can end while capture starts, after disconnect has already
        // taken down everything it could see in the room.
        if !roomAccess.holds(room) {
            await room.localParticipant.unpublishAll()
            await room.disconnect()
            camera.releaseBackgroundProcessors(of: room)
        }
        return issue
    }

    private func mute(_ kind: AVMediaType, in room: Room) async {
        if kind == .audio {
            _ = try? await room.localParticipant.setMicrophone(enabled: false)
        } else {
            camera.cameraNoLongerWanted()
        }
        // Off means all of the publications, including any restored by a reconnect.
        for publication in room.callPublications(of: kind) where !publication.isMuted {
            try? await publication.mute()
        }
    }
}
