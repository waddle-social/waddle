import CoreVideo
import LiveKit
import XCTest

@testable import Waddle

/// What may leave a camera. The blur itself is the SDK's; these are the
/// rules around it, which are what keep a room private.
final class CallBackgroundProcessorTests: XCTestCase {
    /// A blur that does what the test tells it, as the SDK's does: a new frame, or the one it was given.
    private final class FakeBlur: NSObject, VideoProcessor, @unchecked Sendable {
        var blurs = true
        private(set) var processed = 0

        func process(frame: VideoFrame) -> VideoFrame? {
            processed += 1
            return blurs ? CallBackgroundProcessorTests.frame(at: frame.timeStampNs) : frame
        }
    }

    private final class Reports: @unchecked Sendable {
        private let lock = NSLock()
        private var values: [Bool] = []
        func add(_ value: Bool) { lock.withLock { values.append(value) } }
        var all: [Bool] { lock.withLock { values } }
    }

    private static func frame(at nanoseconds: Int64 = 0) -> VideoFrame {
        var buffer: CVPixelBuffer?
        CVPixelBufferCreate(nil, 16, 16, kCVPixelFormatType_32BGRA, nil, &buffer)
        return VideoFrame(
            dimensions: Dimensions(width: 16, height: 16), rotation: ._0, timeStampNs: nanoseconds,
            buffer: CVPixelVideoBuffer(pixelBuffer: buffer!)
        )
    }

    private let reports = Reports()
    private var made: [FakeBlur] = []

    private func makeProcessor(isBlurring: Bool, blurs: Bool = true) -> CallBackgroundProcessor {
        CallBackgroundProcessor(
            isBlurring: isBlurring,
            makeBlur: { [unowned self] in
                let blur = FakeBlur()
                blur.blurs = blurs
                made.append(blur)
                return blur
            },
            failing: { [reports] in reports.add($0) }
        )
    }

    func testWithNoBlurAFrameLeavesAsItIsAndNoBlurIsEvenMade() {
        let processor = makeProcessor(isBlurring: false)
        let frame = Self.frame()
        XCTAssertTrue(processor.process(frame: frame) === frame)
        XCTAssertTrue(made.isEmpty)
    }

    func testABlurredFrameLeavesInPlaceOfTheOneCaptured() {
        let processor = makeProcessor(isBlurring: true)
        let frame = Self.frame()
        let out = processor.process(frame: frame)
        XCTAssertNotNil(out)
        XCTAssertFalse(out === frame, "what leaves is the blurred frame")
    }

    /// The SDK's blur hands the frame back untouched until it has a mask, and whenever it cannot make one.
    func testAFrameTheBlurCouldNotBlurDoesNotLeave() {
        let processor = makeProcessor(isBlurring: true, blurs: false)
        XCTAssertNil(processor.process(frame: Self.frame()), "the room is never sent sharp while blur is on")
        XCTAssertEqual(made.first?.processed, 1)
    }

    func testTheBlurNotWorkingIsSaidOnceAndTakenBackWhenItWorksAgain() {
        let processor = makeProcessor(isBlurring: true, blurs: false)
        for index in 0..<(CallBackgroundProcessor.failureThreshold - 1) {
            XCTAssertNil(processor.process(frame: Self.frame(at: Int64(index) * 33_000_000)))
        }
        XCTAssertEqual(reports.all, [], "a few frames held back is the blur starting")
        let next = Int64(CallBackgroundProcessor.failureThreshold) * 33_000_000
        XCTAssertNil(processor.process(frame: Self.frame(at: next)))
        XCTAssertNil(processor.process(frame: Self.frame(at: next + 33_000_000)))
        XCTAssertEqual(reports.all, [true])

        made.first?.blurs = true
        XCTAssertNotNil(processor.process(frame: Self.frame(at: next + 66_000_000)))
        XCTAssertEqual(reports.all, [true, false])
    }

    func testTurningTheBlurOnStartsAFreshOneAndOffLetsFramesThrough() {
        let processor = makeProcessor(isBlurring: true)
        _ = processor.process(frame: Self.frame())
        processor.setBlurring(false)
        let plain = Self.frame(at: 33_000_000)
        XCTAssertTrue(processor.process(frame: plain) === plain)
        processor.setBlurring(true)
        _ = processor.process(frame: Self.frame(at: 66_000_000))
        XCTAssertEqual(made.count, 2, "no mask of the person from before the blur was turned off")
    }

    /// A camera turned off and on again keeps its capturer: the outline from before is not laid over the new picture.
    func testACameraThatWasOffStartsAFreshBlur() {
        let processor = makeProcessor(isBlurring: true)
        _ = processor.process(frame: Self.frame(at: 0))
        _ = processor.process(frame: Self.frame(at: 33_000_000))
        XCTAssertEqual(made.count, 1)
        _ = processor.process(frame: Self.frame(at: 33_000_000 + CallBackgroundProcessor.staleAfterNanoseconds + 1))
        XCTAssertEqual(made.count, 2)
    }

    func testTryingAgainStartsAFreshBlurAndTakesTheFailureBack() {
        let processor = makeProcessor(isBlurring: true, blurs: false)
        for index in 0...CallBackgroundProcessor.failureThreshold {
            _ = processor.process(frame: Self.frame(at: Int64(index) * 33_000_000))
        }
        XCTAssertEqual(reports.all, [true])
        processor.restart()
        XCTAssertEqual(reports.all, [true, false])
        _ = processor.process(frame: Self.frame(at: Int64(CallBackgroundProcessor.failureThreshold + 1) * 33_000_000))
        XCTAssertEqual(made.count, 2)
    }
}
