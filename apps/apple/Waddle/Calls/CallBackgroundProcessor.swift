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
        lock.lock()
        let changes = on != isBlurring
        if changes { isBlurring = on }
        lock.unlock()
        if changes { startAfresh() }
    }

    /// Starts the blur afresh, as when someone asks to try again.
    func restart() {
        startAfresh()
    }

    private func startAfresh() {
        lock.lock()
        blur = nil
        heldBack = 0
        let hadFailed = hasReportedFailure
        hasReportedFailure = false
        lock.unlock()
        if hadFailed { failing(false) }
    }

    func process(frame: VideoFrame) -> VideoFrame? {
        guard let blur = blurForFrame(at: frame.timeStampNs) else { return frame }
        let blurred = blur.process(frame: frame)
        if let blurred, blurred !== frame {
            if noteBlurred() { failing(false) }
            return blurred
        }
        // Not blurred: it does not leave.
        if noteHeldBack() { failing(true) }
        return nil
    }

    /// The blur to put this frame through, or `nil` when none is wanted.
    /// Made here, on the capturer's queue: it sets up Vision and Metal.
    private func blurForFrame(at timestamp: Int64) -> (any VideoProcessor)? {
        lock.lock()
        if let last = lastFrameAt, timestamp - last > Self.staleAfterNanoseconds { blur = nil }
        lastFrameAt = timestamp
        let wanted = isBlurring
        let existing = blur
        lock.unlock()
        guard wanted else { return nil }
        if let existing { return existing }
        let made = makeBlur()
        lock.lock()
        defer { lock.unlock() }
        if blur == nil { blur = made }
        return blur
    }

    /// A blurred frame got out. True when a failure had been said and is now over.
    private func noteBlurred() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        heldBack = 0
        let hadFailed = hasReportedFailure
        hasReportedFailure = false
        return hadFailed
    }

    /// A frame was held back. True the one time that makes it worth saying.
    private func noteHeldBack() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        heldBack += 1
        guard heldBack >= Self.failureThreshold, !hasReportedFailure else { return false }
        hasReportedFailure = true
        return true
    }
}
