import AVFoundation
import LiveKit
import os
import WaddleKit

/// A camera session can report the reason it has no frames while the publish
/// call is still waiting for its first frame. Keep that reason for this start.
final class CallCameraStartMonitor {
    private let observedFailure = OSAllocatedUnfairLock<CallMediaIssue.Reason?>(initialState: nil)
    private var observers: [any NSObjectProtocol] = []

    init(capturer: VideoCapturer) {
        guard let session = (capturer as? CameraCapturer)?.captureSession else { return }
        let failure = observedFailure
        let center = NotificationCenter.default
        observers.append(
            center.addObserver(forName: AVCaptureSession.runtimeErrorNotification, object: session, queue: nil) { note in
                guard let error = note.userInfo?[AVCaptureSessionErrorKey] as? NSError else { return }
                let reason = CallDeviceStartError.reason(for: error)
                if reason != .failed { failure.withLock { $0 = reason } }
            }
        )
        #if os(iOS)
        observers.append(
            center.addObserver(forName: AVCaptureSession.wasInterruptedNotification, object: session, queue: nil) { note in
                guard let raw = note.userInfo?[AVCaptureSessionInterruptionReasonKey] as? NSNumber,
                      AVCaptureSession.InterruptionReason(rawValue: raw.intValue) == .videoDeviceInUseByAnotherClient
                else { return }
                failure.withLock { $0 = .inUse }
            }
        )
        #endif
    }

    deinit {
        observers.forEach(NotificationCenter.default.removeObserver)
    }

    /// A reused publication stays muted until the camera produces a frame.
    /// LiveKit's unmute starts capture but does not wait for the first frame.
    @MainActor
    static func resume(_ publication: LocalTrackPublication, while isCurrent: () -> Bool) async -> CallMediaIssue.Reason? {
        guard isCurrent(), let track = publication.track as? LocalVideoTrack else { return nil }
        let monitor = Self(capturer: track.capturer)
        do {
            try await track.start()
            let failure = await monitor.waitForFrame(from: track.capturer, while: isCurrent)
            if failure == nil, isCurrent() {
                try await publication.unmute()
                return nil
            }
            await stop(track)
            return failure
        } catch {
            let failure = monitor.reason(for: error)
            await stop(track)
            return failure
        }
    }

    @MainActor
    private func waitForFrame(from capturer: VideoCapturer, while isCurrent: () -> Bool) async -> CallMediaIssue.Reason? {
        // Match the SDK's first-frame deadline for a new publication.
        let deadline = ContinuousClock.now + .seconds(10)
        while isCurrent(), capturer.dimensions == nil {
            if let failure = observedFailure.withLock({ $0 }) { return failure }
            guard ContinuousClock.now < deadline else { return .failed }
            do { try await Task.sleep(for: .milliseconds(50)) } catch { return .failed }
        }
        return nil
    }

    static func stop(_ track: LocalVideoTrack) async {
        try? await track.stop()
        // A throw during start can leave a capturer started before the
        // track records its own state. Balance that partial start too.
        if track.capturer.captureState == .started { _ = try? await track.capturer.stopCapture() }
    }

    func reason(for error: Error) -> CallMediaIssue.Reason {
        let reason = CallDeviceStartError.reason(for: error)
        guard reason == .failed, let liveKit = error as? LiveKitError,
              liveKit.type == .timedOut || liveKit.type == .capturerDimensionsNotResolved
        else { return reason }
        return observedFailure.withLock { $0 } ?? reason
    }
}
