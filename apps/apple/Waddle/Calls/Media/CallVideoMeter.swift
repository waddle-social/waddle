import LiveKit
import Observation
import WaddleKit

@MainActor
final class CallVideoMeter {
    /// One video stream of the call and the track that carries it.
    struct VideoStream {
        let key: String
        let label: String
        let source: CallVideoSource
        let direction: CallVideoSample.Direction
        let track: Track
    }

    private let roomAccess: CallRoomAccess

    init(roomAccess: CallRoomAccess) {
        self.roomAccess = roomAccess
    }

    /// The tracks asked to report statistics. Asking one again restarts its
    /// timer, so each is asked once.
    @ObservationIgnored private var measuredTracks: [ObjectIdentifier: Track] = [:]
    /// How many are looking: a Mac can have the settings open in two
    /// windows, and closing one must not blind the other.
    @ObservationIgnored private var videoWatchers = 0
    private var measuresVideo: Bool { videoWatchers > 0 }

    func setMeasuresVideo(_ on: Bool, videoStreams: @MainActor () -> [VideoStream]) async {
        videoWatchers = max(0, videoWatchers + (on ? 1 : -1))
        guard measuresVideo else {
            let tracks = Array(measuredTracks.values)
            measuredTracks = [:]
            for track in tracks { await track.set(reportStatistics: false) }
            return
        }
        await measureNewVideoTracks(videoStreams: videoStreams)
    }

    func roomWentAway() {
        measuredTracks = [:]
        videoWatchers = 0
    }

    private func measureNewVideoTracks(videoStreams: @MainActor () -> [VideoStream]) async {
        let measuring = roomAccess.room
        for stream in videoStreams() where measuredTracks[ObjectIdentifier(stream.track)] == nil {
            measuredTracks[ObjectIdentifier(stream.track)] = stream.track
            await stream.track.set(reportStatistics: true)
            // The settings closed, or the call ended, while that one was being
            // switched on: it goes off again and nothing more is switched on.
            guard measuresVideo, let measuring, roomAccess.holds(measuring) else {
                measuredTracks[ObjectIdentifier(stream.track)] = nil
                await stream.track.set(reportStatistics: false)
                return
            }
        }
        // A track that is no longer a stream, as a camera turned off, stops
        // being measured: muting does not stop the SDK's timer, and a track
        // only dropped from here would go on polling for the rest of the call.
        let live = Set(videoStreams().map { ObjectIdentifier($0.track) })
        for (id, track) in measuredTracks where !live.contains(id) {
            measuredTracks[id] = nil
            await track.set(reportStatistics: false)
        }
    }

    func videoSamples(videoStreams: @MainActor () -> [VideoStream]) async -> [CallVideoSample] {
        guard measuresVideo else { return [] }
        // A camera turned on while someone is looking is measured from then on.
        await measureNewVideoTracks(videoStreams: videoStreams)
        guard measuresVideo else { return [] }
        return videoStreams().map(Self.sample(of:))
    }

    private static func sample(of stream: VideoStream) -> CallVideoSample {
        var sample = CallVideoSample(
            key: stream.key, label: stream.label, source: stream.source, direction: stream.direction, timestamp: 0
        )
        guard let statistics = stream.track.statistics else { return sample }
        let candidate: LocalIceCandidateStatistics? = statistics.localIceCandidate
        sample.candidateType = candidate?.candidateType?.rawValue
        // A relay's own protocol is its leg to the peer, nearly always UDP;
        // what tells a TCP fallback is how we reach the relay (as web `iceCandidatePath`).
        if candidate?.candidateType == .relay {
            sample.transport = candidate?.relayProtocol.map { $0 == .udp ? "udp" : "tcp" }
        } else {
            sample.transport = candidate?.protocol
        }
        switch stream.direction {
        case .sent: fill(&sample, sentBy: stream.track, statistics)
        case .received: fill(&sample, received: statistics)
        }
        return sample
    }

    /// The codec a stream is carried in: the one its own entry names. A
    /// report also lists the retransmission codec, which is "video/rtx" and
    /// must never be the answer; without a name to go by, it is the first that is not.
    private static func codec(_ id: String?, in statistics: TrackStatistics) -> String? {
        let named: String? = id.flatMap { id in statistics.codec.first { $0.id == id }?.mimeType }
        if let named { return named }
        return statistics.codec.compactMap(\.mimeType).first { type in
            type.hasPrefix("video/") && type.lowercased() != "video/rtx"
        }
    }

    private static func fill(_ sample: inout CallVideoSample, sentBy track: Track, _ statistics: TrackStatistics) {
        // Simulcast sends several sizes: the figures are the largest one's, the bytes all of them.
        let layers: [OutboundRtpStreamStatistics] = statistics.outboundRtpStream
        func area(_ layer: OutboundRtpStreamStatistics) -> UInt { (layer.frameWidth ?? 0) * (layer.frameHeight ?? 0) }
        let top: OutboundRtpStreamStatistics? = layers.max { area($0) < area($1) }
        sample.width = top?.frameWidth.map { Int($0) }
        sample.height = top?.frameHeight.map { Int($0) }
        sample.framesPerSecond = top?.framesPerSecond
        sample.timestamp = top?.timestamp ?? 0
        sample.codec = codec(top?.codecId, in: statistics)
        // A backup codec for those who cannot decode the first has a sender of its own.
        let backups: [OutboundRtpStreamStatistics] = track.simulcastStatistics.values.flatMap(\.outboundRtpStream)
        let sent: [UInt64] = (layers + backups).compactMap(\.bytesSent)
        sample.bytes = sent.isEmpty ? nil : sent.reduce(0, +)
        // The far end's word on what arrived, over every layer as the web
        // counts it: one layer's report alone jumps about.
        let reports: [RemoteInboundRtpStreamStatistics] = statistics.remoteInboundRtpStream
        let lost: Int64 = reports.compactMap(\.packetsLost).reduce(0, +)
        let packets: UInt64 = layers.compactMap(\.packetsSent).reduce(0, +)
        if !reports.isEmpty, packets > 0 { sample.lossFraction = Double(max(lost, 0)) / Double(packets) }
        sample.roundTrip = reports.compactMap(\.roundTripTime).max()
    }

    private static func fill(_ sample: inout CallVideoSample, received statistics: TrackStatistics) {
        let stream: InboundRtpStreamStatistics? = statistics.inboundRtpStream.first
        sample.width = stream?.frameWidth.map { Int($0) }
        sample.height = stream?.frameHeight.map { Int($0) }
        sample.framesPerSecond = stream?.framesPerSecond
        sample.timestamp = stream?.timestamp ?? 0
        sample.bytes = stream?.bytesReceived
        sample.codec = codec(stream?.codecId, in: statistics)
        if let lost = stream?.packetsLost, let received = stream?.packetsReceived {
            let missing: Int64 = max(lost, 0)
            let total: Int64 = Int64(received) + missing
            if total > 0 { sample.lossFraction = Double(missing) / Double(total) }
        }
        sample.roundTrip = statistics.iceCandidatePair.first?.currentRoundTripTime
    }
}
