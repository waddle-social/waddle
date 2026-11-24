import Foundation

/// One reading of one video stream in a call, as the media engine reports it.
public struct CallVideoSample: Equatable, Sendable {
    public enum Direction: Equatable, Sendable {
        case sent, received
    }

    /// Stable for the stream: whose it is, what it shows, and which way it goes.
    public let key: String
    /// "You", or the person's name.
    public let label: String
    public let source: CallVideoSource
    public let direction: Direction
    public var width: Int?
    public var height: Int?
    public var framesPerSecond: Double?
    /// Everything sent or received on the stream so far.
    public var bytes: UInt64?
    /// When the engine took the reading, in its own clock and unit.
    public var timestamp: Double
    /// The share of packets lost, from 0 to 1.
    public var lossFraction: Double?
    public var roundTrip: TimeInterval?
    /// As negotiated, such as "video/VP9".
    public var codec: String?
    /// How the media leaves this device: "host", "srflx", "prflx" or "relay".
    public var candidateType: String?
    /// "udp" or "tcp".
    public var transport: String?

    public init(
        key: String, label: String, source: CallVideoSource, direction: Direction, width: Int? = nil, height: Int? = nil,
        framesPerSecond: Double? = nil, bytes: UInt64? = nil, timestamp: Double, lossFraction: Double? = nil,
        roundTrip: TimeInterval? = nil, codec: String? = nil, candidateType: String? = nil, transport: String? = nil
    ) {
        self.key = key
        self.label = label
        self.source = source
        self.direction = direction
        self.width = width
        self.height = height
        self.framesPerSecond = framesPerSecond
        self.bytes = bytes
        self.timestamp = timestamp
        self.lossFraction = lossFraction
        self.roundTrip = roundTrip
        self.codec = codec
        self.candidateType = candidateType
        self.transport = transport
    }
}

/// A video stream's figures as the call settings show them (web
/// `describeVideoStats`): what is not known is a dash, never a guess.
public struct CallVideoStat: Equatable, Identifiable, Sendable {
    public let id: String
    /// "You · Camera", "linus · Screen".
    public let title: String
    public let resolution: String
    public let framesPerSecond: String
    public let bitrate: String
    public let loss: String
    public let roundTrip: String
    public let codec: String
    public let path: String

    static let unknown = "—"
}

/// Turns readings into figures. A bitrate is the bytes between two
/// readings of a stream over the time between them, so the readings are
/// kept from one to the next.
public struct CallVideoStatsTracker: Equatable, Sendable {
    private var last: [String: CallVideoSample] = [:]
    private var bitrates: [String: Int] = [:]

    public init() { /* No readings yet. */ }

    /// The figures for `samples`, in their order. A stream no longer among
    /// them is forgotten: a camera turned off and on starts its bitrate afresh.
    public mutating func update(with samples: [CallVideoSample]) -> [CallVideoStat] {
        var kept: [String: CallVideoSample] = [:]
        var rates: [String: Int] = [:]
        for sample in samples {
            if let before = last[sample.key], let rate = Self.kilobitsPerSecond(from: before, to: sample) {
                rates[sample.key] = rate
                kept[sample.key] = sample
            } else if let before = last[sample.key], before.timestamp == sample.timestamp || sample.bytes == nil {
                // The engine has not read the stream again yet: what is known stands.
                rates[sample.key] = bitrates[sample.key]
                kept[sample.key] = before
            } else {
                kept[sample.key] = sample
            }
        }
        last = kept
        bitrates = rates
        return samples.map { Self.describe($0, kilobitsPerSecond: rates[$0.key]) }
    }

    /// `nil` when there is nothing to measure between the two: the same
    /// reading twice, a counter that went backwards (a restarted stream), or no byte count.
    static func kilobitsPerSecond(from before: CallVideoSample, to after: CallVideoSample) -> Int? {
        guard let earlier = before.bytes, let later = after.bytes, later >= earlier else { return nil }
        let seconds = Self.seconds(after.timestamp) - Self.seconds(before.timestamp)
        guard seconds > 0 else { return nil }
        return Int((Double(later - earlier) * 8 / seconds / 1000).rounded())
    }

    /// Engines stamp readings in seconds, milliseconds or microseconds
    /// since 1970; which one shows in the size of the number.
    public static func seconds(_ timestamp: Double) -> Double {
        switch timestamp {
        case 1e14...: timestamp / 1_000_000
        case 1e11...: timestamp / 1000
        default: timestamp
        }
    }

    static func describe(_ sample: CallVideoSample, kilobitsPerSecond: Int?) -> CallVideoStat {
        let unknown = CallVideoStat.unknown
        return CallVideoStat(
            id: sample.key,
            title: "\(sample.label) · \(sample.source == .camera ? "Camera" : "Screen")",
            resolution: sample.width.flatMap { width in sample.height.map { "\(width)×\($0)" } } ?? unknown,
            framesPerSecond: sample.framesPerSecond.map { "\(Int($0.rounded())) fps" } ?? unknown,
            bitrate: kilobitsPerSecond.map(Self.bitrate) ?? unknown,
            loss: sample.lossFraction.map { "\(Self.oneDecimal(min(max($0, 0), 1) * 100))%" } ?? unknown,
            roundTrip: sample.roundTrip.map { "\(Int(($0 * 1000).rounded())) ms" } ?? unknown,
            codec: sample.codec.map { $0.split(separator: "/").last.map(String.init) ?? $0 } ?? unknown,
            path: sample.candidateType.map { type in sample.transport.map { "\(type) · \($0)" } ?? type } ?? unknown
        )
    }

    /// As the web's `formatBitrate`.
    static func bitrate(_ kilobitsPerSecond: Int) -> String {
        kilobitsPerSecond >= 1000 ? "\(oneDecimal(Double(kilobitsPerSecond) / 1000)) Mbps" : "\(kilobitsPerSecond) kbps"
    }

    /// One decimal with a point, whatever the region: these are engineering figures.
    private static func oneDecimal(_ value: Double) -> String {
        let tenths = Int((value * 10).rounded())
        return "\(tenths / 10).\(tenths % 10)"
    }
}
