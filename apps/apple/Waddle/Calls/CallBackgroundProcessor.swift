import Foundation
import LiveKit

/// Stands between a camera and everything its picture goes to, the
/// preview and the encoder alike, and decides what leaves: the frame as
/// it is, the frame with the room behind us blurred, or nothing.
///
/// It is on a camera's capturer from the moment the capturer is made,
/// whether or not a blur is wanted, so there is never a frame of a new
/// camera that got out before the blur was attached. And it fails closed:
/// the SDK's blur hands back the frame it was given whenever it has no
/// mask of the person yet or cannot make one, which would send the room
/// sharp under a control that says Blur. Such a frame is held back.
///
/// `process` is called on the capturer's own serial queue; `setBlurring`
/// from the main actor.
final class CallBackgroundProcessor: NSObject, VideoProcessor, @unchecked Sendable {
    /// Frames held back in a row before it is said that the blur is not
    /// working: about three seconds of a camera. Fewer is the blur starting.
    static let failureThreshold = 90
    /// A camera that was off this long has moved on: the person's outline
    /// from before must not be laid over the new picture.
    static let staleAfterNanoseconds: Int64 = 1_000_000_000

    private let lock = NSLock()
    private var isBlurring: Bool
    private var blur: (any VideoProcessor)?
    private var lastFrameAt: Int64?
    private var heldBack = 0
    private var hasReportedFailure = false
    private let makeBlur: @Sendable () -> any VideoProcessor
    /// Called from the capturer's queue: `true` when frames have been held
    /// back for `failureThreshold` in a row, `false` when a blurred one gets out again.
    private let failing: @Sendable (Bool) -> Void

    init(
        isBlurring: Bool,
        makeBlur: @escaping @Sendable () -> any VideoProcessor = { BackgroundBlurVideoProcessor() },
        failing: @escaping @Sendable (Bool) -> Void
    ) {
        self.isBlurring = isBlurring
        self.makeBlur = makeBlur
        self.failing = failing
    }

    /// Turning the blur on starts from nothing: no mask from an earlier time.
    func setBlurring(_ on: Bool) {
        let recovered = lock.withLock { () -> Bool in
            guard on != isBlurring else { return false }
            isBlurring = on
            blur = nil
            heldBack = 0
            defer { hasReportedFailure = false }
            return hasReportedFailure
        }
        if recovered { failing(false) }
    }

    /// Starts the blur afresh, as when someone asks to try again.
    func restart() {
        let recovered = lock.withLock { () -> Bool in
            blur = nil
            heldBack = 0
            defer { hasReportedFailure = false }
            return hasReportedFailure
        }
        if recovered { failing(false) }
    }

    func process(frame: VideoFrame) -> VideoFrame? {
        let (blurring, current, isStale) = lock.withLock { () -> (Bool, (any VideoProcessor)?, Bool) in
            let stale = lastFrameAt.map { frame.timeStampNs - $0 > Self.staleAfterNanoseconds } ?? false
            lastFrameAt = frame.timeStampNs
            if stale { blur = nil }
            return (isBlurring, blur, stale)
        }
        guard blurring else { return frame }
        _ = isStale
        // Made here, off the main thread: it sets up Vision and Metal.
        let blur = current ?? {
            let made = makeBlur()
            return lock.withLock {
                if self.blur == nil { self.blur = made }
                return self.blur ?? made
            }
        }()
        if let blurred = blur.process(frame: frame), blurred !== frame {
            let recovered = lock.withLock { () -> Bool in
                heldBack = 0
                defer { hasReportedFailure = false }
                return hasReportedFailure
            }
            if recovered { failing(false) }
            return blurred
        }
        // Not blurred: it does not leave.
        let reportsNow = lock.withLock { () -> Bool in
            heldBack += 1
            guard heldBack >= Self.failureThreshold, !hasReportedFailure else { return false }
            hasReportedFailure = true
            return true
        }
        if reportsNow { failing(true) }
        return nil
    }
}
