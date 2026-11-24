import Testing

@testable import WaddleKit

@Suite("Call participant rows") struct CallParticipantRowsTests {
    private static let me = "ada@waddle.test/mac"
    private static let linus = "linus@waddle.test/web"
    private static let grace = "grace@waddle.test/phone"

    private static func tile(
        _ identity: String, label: String, isSelf: Bool = false, isMuted: Bool = false, isHandRaised: Bool = false,
        isSpeaking: Bool = false, hasVideo: Bool = false, source: CallVideoSource = .camera
    ) -> CallStageTile {
        CallStageTile(
            identity: identity, source: source, label: label, isSelf: isSelf, isMuted: isMuted,
            isHandRaised: isHandRaised, isSpeaking: isSpeaking, hasVideo: hasVideo
        )
    }

    @Test func weComeFirstAndHaveNoVolumeOfOurOwn() {
        let rows = CallParticipantRows.make(
            tiles: [Self.tile(Self.linus, label: "linus"), Self.tile(Self.me, label: "You", isSelf: true, isMuted: true)],
            audio: [
                CallParticipantAudio(identity: Self.me, hasMicrophone: true, volume: 1),
                CallParticipantAudio(identity: Self.linus, hasMicrophone: true, volume: 1.4),
            ]
        )
        #expect(rows.map(\.id) == [Self.me, Self.linus])
        #expect(rows[0].volume == nil)
        #expect(rows[0].devices?.isMicrophoneOn == false)
        #expect(rows[1].volume == 1.4)
        #expect(rows[1].devices?.isMicrophoneOn == true)
    }

    @Test func theRoomsWordOnAMuteWinsOverAPublishedTrack() {
        let rows = CallParticipantRows.make(
            tiles: [Self.tile(Self.linus, label: "linus", isMuted: true, hasVideo: true), Self.tile(Self.grace, label: "grace")],
            audio: [
                CallParticipantAudio(identity: Self.linus, hasMicrophone: true, volume: 1),
                CallParticipantAudio(identity: Self.grace, hasMicrophone: false, volume: 1),
            ]
        )
        #expect(rows[0].devices == .init(isMicrophoneOn: false, isCameraOn: true))
        #expect(rows[1].devices == .init(isMicrophoneOn: false, isCameraOn: false))
    }

    @Test func aDeviceNotYetInTheMediaSessionShowsNoDevicesAndNoVolume() {
        let rows = CallParticipantRows.make(
            tiles: [Self.tile(Self.me, label: "You", isSelf: true), Self.tile(Self.linus, label: "linus", isHandRaised: true)],
            audio: []
        )
        #expect(rows.allSatisfy { $0.devices == nil && $0.volume == nil })
        #expect(rows[1].isHandRaised)
    }

    @Test func aScreenShareIsNotAnotherPerson() {
        let rows = CallParticipantRows.make(
            tiles: [Self.tile(Self.linus, label: "linus"), Self.tile(Self.linus, label: "linus", source: .screen)],
            audio: []
        )
        #expect(rows.count == 1)
    }

    @Test func resetAllTouchesOnlyVolumesThatMoved() {
        let rows = CallParticipantRows.make(
            tiles: [
                Self.tile(Self.me, label: "You", isSelf: true), Self.tile(Self.linus, label: "linus"),
                Self.tile(Self.grace, label: "grace"),
            ],
            audio: [
                CallParticipantAudio(identity: Self.me, hasMicrophone: true, volume: 1),
                CallParticipantAudio(identity: Self.linus, hasMicrophone: true, volume: 0.3),
                CallParticipantAudio(identity: Self.grace, hasMicrophone: true, volume: 1),
            ]
        )
        #expect(CallParticipantRows.changedVolumes(in: rows) == [Self.linus])
    }

    @Test func aRowSaysWhatItShows() {
        let rows = CallParticipantRows.make(
            tiles: [Self.tile(Self.linus, label: "linus", isHandRaised: true, isSpeaking: true, hasVideo: true)],
            audio: [CallParticipantAudio(identity: Self.linus, hasMicrophone: true, volume: 1)]
        )
        #expect(rows[0].spokenLabel == "linus, speaking, hand raised, microphone on, camera on")
    }
}
