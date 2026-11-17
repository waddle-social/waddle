import Foundation

/// The two sounds of a call, synthesised rather than shipped as audio
/// files, as the web client does: a short rising phrase for a call coming
/// in, and a single long note for the other side ringing.
public enum CallTone: CaseIterable, Sendable {
    case ring
    case ringback

    /// One stretch of the pattern: a note, or silence when `frequency` is nil.
    public struct Segment: Hashable, Sendable {
        public let frequency: Double?
        public let duration: TimeInterval
    }

    public static let sampleRate = 22_050
    /// Full scale is 1. Quiet enough to sit under a conversation.
    public static let peak = 0.22
    /// Each note fades in and out over this long, so none starts or stops
    /// with a click.
    static let fade: TimeInterval = 0.02

    /// The pattern, which ends in silence and is played on a loop.
    public var segments: [Segment] {
        switch self {
        case .ring:
            [.init(frequency: 659.25, duration: 0.16), .init(frequency: nil, duration: 0.05),
             .init(frequency: 880, duration: 0.16), .init(frequency: nil, duration: 0.05),
             .init(frequency: 1_046.5, duration: 0.32), .init(frequency: nil, duration: 1.6)]
        case .ringback:
            [.init(frequency: 440, duration: 1), .init(frequency: nil, duration: 3)]
        }
    }

    public var duration: TimeInterval { segments.reduce(0) { $0 + $1.duration } }

    /// The pattern once through, as a 16-bit mono WAV file.
    public func waveData() -> Data {
        var pcm: [Int16] = []
        for segment in segments {
            let count = Int((segment.duration * Double(Self.sampleRate)).rounded())
            guard let frequency = segment.frequency else {
                pcm.append(contentsOf: repeatElement(0, count: count))
                continue
            }
            let fade = max(1, Int(Self.fade * Double(Self.sampleRate)))
            for index in 0..<count {
                let envelope = min(1, Double(index) / Double(fade), Double(count - 1 - index) / Double(fade))
                let wave = sin(2 * .pi * frequency * Double(index) / Double(Self.sampleRate))
                pcm.append(Int16(wave * envelope * Self.peak * Double(Int16.max)))
            }
        }
        return Self.wave(pcm)
    }

    private static func wave(_ pcm: [Int16]) -> Data {
        let bytes = UInt32(pcm.count * 2)
        var data = Data()
        func append<T: FixedWidthInteger>(_ value: T) {
            withUnsafeBytes(of: value.littleEndian) { data.append(contentsOf: $0) }
        }
        data.append(contentsOf: Array("RIFF".utf8))
        append(36 + bytes)
        data.append(contentsOf: Array("WAVEfmt ".utf8))
        append(UInt32(16))                  // size of the format block
        append(UInt16(1))                   // PCM
        append(UInt16(1))                   // mono
        append(UInt32(sampleRate))
        append(UInt32(sampleRate * 2))      // bytes per second
        append(UInt16(2))                   // bytes per frame
        append(UInt16(16))                  // bits per sample
        data.append(contentsOf: Array("data".utf8))
        append(bytes)
        for sample in pcm { append(sample) }
        return data
    }
}
