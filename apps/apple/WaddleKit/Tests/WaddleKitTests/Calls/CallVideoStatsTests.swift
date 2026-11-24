import Testing

@testable import WaddleKit

@Suite("Call video stats") struct CallVideoStatsTests {
    private static func sample(
        key: String = "you:camera:sent", label: String = "You", source: CallVideoSource = .camera,
        direction: CallVideoSample.Direction = .sent, bytes: UInt64? = 0, timestamp: Double = 1_700_000_000_000
    ) -> CallVideoSample {
        CallVideoSample(key: key, label: label, source: source, direction: direction, bytes: bytes, timestamp: timestamp)
    }

    @Test func whatIsNotKnownIsADash() {
        var tracker = CallVideoStatsTracker()
        let rows = tracker.update(with: [Self.sample(bytes: nil)])
        #expect(rows == [CallVideoStat(
            id: "you:camera:sent", title: "You · Camera", resolution: "—", framesPerSecond: "—", bitrate: "—", loss: "—",
            roundTrip: "—", codec: "—", path: "—"
        )])
    }

    @Test func everyFigureIsWrittenAsTheWebWritesIt() {
        var tracker = CallVideoStatsTracker()
        var reading = Self.sample(key: "linus:screen:received", label: "linus", source: .screen, direction: .received)
        reading.width = 2560
        reading.height = 1440
        reading.framesPerSecond = 29.6
        reading.lossFraction = 0.0123
        reading.roundTrip = 0.0416
        reading.codec = "video/VP9"
        reading.candidateType = "relay"
        reading.transport = "tcp"
        let row = tracker.update(with: [reading])[0]
        #expect(row.title == "linus · Screen")
        #expect(row.resolution == "2560 × 1440")
        #expect(row.framesPerSecond == "30 fps")
        #expect(row.loss == "1.2%")
        #expect(row.roundTrip == "42 ms")
        #expect(row.codec == "VP9")
        #expect(row.path == "relay · tcp")
    }

    @Test func aPathWithNoTransportIsItsTypeAlone() {
        var tracker = CallVideoStatsTracker()
        var reading = Self.sample()
        reading.candidateType = "host"
        #expect(tracker.update(with: [reading])[0].path == "host")
    }

    @Test func theBitrateIsTheBytesBetweenTwoReadingsOverTheTimeBetweenThem() {
        var tracker = CallVideoStatsTracker()
        #expect(tracker.update(with: [Self.sample(bytes: 1_000_000, timestamp: 1_700_000_000_000)])[0].bitrate == "—")
        // 150 000 bytes in one second is 1200 kbps.
        #expect(tracker.update(with: [Self.sample(bytes: 1_150_000, timestamp: 1_700_000_001_000)])[0].bitrate == "1.2 Mbps")
        // 50 000 bytes in the next second is 400 kbps.
        #expect(tracker.update(with: [Self.sample(bytes: 1_200_000, timestamp: 1_700_000_002_000)])[0].bitrate == "400 kbps")
    }

    @Test func aReadingTheEngineHasNotRenewedKeepsTheBitrateItHad() {
        var tracker = CallVideoStatsTracker()
        _ = tracker.update(with: [Self.sample(bytes: 0, timestamp: 1_700_000_000_000)])
        _ = tracker.update(with: [Self.sample(bytes: 125_000, timestamp: 1_700_000_001_000)])
        // Asked again before the engine's next reading: not zero, and not a dash.
        #expect(tracker.update(with: [Self.sample(bytes: 125_000, timestamp: 1_700_000_001_000)])[0].bitrate == "1.0 Mbps")
        // The next real reading is measured from the last real one.
        #expect(tracker.update(with: [Self.sample(bytes: 250_000, timestamp: 1_700_000_002_000)])[0].bitrate == "1.0 Mbps")
    }

    @Test func aStreamThatStartsAgainStartsItsBitrateAgain() {
        var tracker = CallVideoStatsTracker()
        _ = tracker.update(with: [Self.sample(bytes: 900_000, timestamp: 1_700_000_000_000)])
        // The counter went back: a new stream under the same key.
        #expect(tracker.update(with: [Self.sample(bytes: 10_000, timestamp: 1_700_000_001_000)])[0].bitrate == "—")
        #expect(tracker.update(with: [Self.sample(bytes: 135_000, timestamp: 1_700_000_002_000)])[0].bitrate == "1.0 Mbps")
    }

    @Test func aStreamThatLeftIsForgotten() {
        var tracker = CallVideoStatsTracker()
        _ = tracker.update(with: [Self.sample(bytes: 0, timestamp: 1_700_000_000_000)])
        #expect(tracker.update(with: []).isEmpty)
        #expect(tracker.update(with: [Self.sample(bytes: 500_000, timestamp: 1_700_000_005_000)])[0].bitrate == "—")
    }

    @Test(arguments: [(1_700_000_000.0, 1_700_000_001.0), (1_700_000_000_000.0, 1_700_000_001_000.0),
                      (1_700_000_000_000_000.0, 1_700_000_001_000_000.0)])
    func timestampsInSecondsMillisecondsOrMicrosecondsMeasureTheSame(from: Double, to: Double) {
        let rate = CallVideoStatsTracker.kilobitsPerSecond(
            from: Self.sample(bytes: 0, timestamp: from), to: Self.sample(bytes: 125_000, timestamp: to)
        )
        #expect(rate == 1000)
    }

    @Test func rowsKeepTheOrderOfTheStreams() {
        var tracker = CallVideoStatsTracker()
        let rows = tracker.update(with: [
            Self.sample(key: "a", label: "You"), Self.sample(key: "b", label: "linus", direction: .received),
        ])
        #expect(rows.map(\.id) == ["a", "b"])
    }
}
