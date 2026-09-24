import Foundation

/// Frame timing for playing an animated GIF as a list of equal-length
/// slots, which is how UIKit plays an animated image.
public enum AnimationFrameTiming {
    /// Browsers play a missing, zero or near-zero GIF delay at 100 ms.
    public static let fallbackDelay = 0.1
    public static let minimumDelay = 0.02
    /// GIF delays run to 655 s; longer than this is held for this long.
    public static let maximumDelay = 10.0
    /// The most slots a playback holds, however its delays divide.
    public static let maximumSlots = 1000

    /// Uniform playback: slot `i` shows frame `i`'s image `repeats[i]`
    /// times, each slot lasting `unit` seconds.
    public struct Playback: Equatable, Sendable {
        public let unit: Double
        public let repeats: [Int]

        public var duration: Double { unit * Double(repeats.reduce(0, +)) }
    }

    /// The delay a frame plays for, in seconds.
    public static func normalized(_ delay: Double?) -> Double {
        guard let delay, delay.isFinite, delay >= minimumDelay else { return fallbackDelay }
        return min(delay, maximumDelay)
    }

    /// Keep every `step`-th frame so at most `maxFrames` frames decode.
    public static func step(frameCount: Int, maxFrames: Int) -> Int {
        let limit = max(maxFrames, 1)
        return max(1, (frameCount + limit - 1) / limit)
    }

    /// Delays of the kept frames (every `step`-th, starting at 0): each
    /// plays for the frames it stands in for, so the loop keeps its length.
    public static func mergedDelays(_ delays: [Double], step: Int) -> [Double] {
        let step = max(step, 1)
        return Swift.stride(from: 0, to: delays.count, by: step).map { start in
            delays[start..<min(start + step, delays.count)].reduce(0, +)
        }
    }

    /// Frames of differing delays as equal slots: the slot is the greatest
    /// common divisor of the delays in centiseconds, GIF's time unit. When
    /// that would take more than `maxSlots` slots, the slot grows to fit
    /// and each frame's delay rounds to it (at least one slot per frame).
    public static func playback(_ delays: [Double], maxSlots: Int = maximumSlots) -> Playback {
        let centiseconds = delays.map { max(1, Int(($0 * 100).rounded())) }
        let divisor = centiseconds.reduce(0, greatestCommonDivisor)
        guard divisor > 0 else { return Playback(unit: fallbackDelay, repeats: []) }
        let total = centiseconds.reduce(0, +)
        let limit = max(maxSlots, 1)
        let unit = total / divisor <= limit ? divisor : (total + limit - 1) / limit
        return Playback(
            unit: Double(unit) / 100,
            repeats: centiseconds.map { max(1, ($0 + unit / 2) / unit) }
        )
    }

    private static func greatestCommonDivisor(_ lhs: Int, _ rhs: Int) -> Int {
        rhs == 0 ? lhs : greatestCommonDivisor(rhs, lhs % rhs)
    }
}
