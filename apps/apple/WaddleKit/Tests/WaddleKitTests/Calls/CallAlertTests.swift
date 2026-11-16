import Foundation
import Testing
import WaddleKit

@Suite("Call alerts") struct CallAlertTests {
    private let peer = JID(parsing: "bob@waddle.test/phone")!
    private let audio = CallMedia(audio: true, video: false)

    private func alert(_ state: CallState, doNotDisturb: Bool = false) -> CallAlert? {
        CallAlert(state: state, isDoNotDisturb: doNotDisturb)
    }

    @Test func anIncomingCallRingsWithWhoIsCallingAndHow() {
        let state = CallState.incoming(IncomingCall(from: peer, sid: "s1", media: CallMedia(audio: true, video: true)))
        #expect(alert(state) == .ring(.init(sid: "s1", from: peer.bare, isVideo: true)))
    }

    @Test func aCallBeingAnsweredHasStoppedRinging() {
        #expect(alert(.incoming(IncomingCall(from: peer, sid: "s1", media: audio, isAccepting: true))) == nil)
    }

    @Test func doNotDisturbSilencesTheRingButNotOurOwnCall() {
        #expect(alert(.incoming(IncomingCall(from: peer, sid: "s1", media: audio)), doNotDisturb: true) == nil)
        let ringing = CallState.outgoing(OutgoingCall(to: peer.bare, sid: "s2", media: audio, isRinging: true))
        #expect(alert(ringing, doNotDisturb: true) == .ringback(sid: "s2"))
    }

    @Test func ringbackPlaysOnlyWhileTheOtherSideIsRinging() {
        // Until a device of theirs says it is ringing there is nothing to echo.
        #expect(alert(.outgoing(OutgoingCall(to: peer.bare, sid: "s2", media: audio))) == nil)
        #expect(alert(.outgoing(OutgoingCall(to: peer.bare, sid: "s2", media: audio, isRinging: true))) == .ringback(sid: "s2"))
        // They picked up: the session is being set up.
        #expect(alert(.outgoing(OutgoingCall(to: peer.bare, sid: "s2", media: audio, isRinging: true, responder: peer))) == nil)
    }

    @Test func nothingElseMakesASound() {
        #expect(alert(.idle) == nil)
        #expect(alert(.ended(sid: "s1", reason: .rejected)) == nil)
        #expect(alert(.joiningGroup(GroupCallAttempt(
            room: BareJID(parsing: "room@muc.waddle.test")!, sid: "g1", media: audio, nick: "alice"))) == nil)
    }
}

@Suite("Call tones") struct CallToneTests {
    private func samples(_ data: Data) -> [Int16] {
        data.dropFirst(44).withUnsafeBytes { raw in
            stride(from: 0, to: raw.count - 1, by: 2).map { Int16(littleEndian: raw.loadUnaligned(fromByteOffset: $0, as: Int16.self)) }
        }
    }

    @Test(arguments: CallTone.allCases) func aToneIsAWaveFileOfItsWholePattern(tone: CallTone) {
        let data = tone.waveData()
        #expect(String(decoding: data.prefix(4), as: UTF8.self) == "RIFF")
        #expect(String(decoding: data[8..<16], as: UTF8.self) == "WAVEfmt ")
        let expected = Int((tone.duration * Double(CallTone.sampleRate)).rounded())
        #expect(abs(samples(data).count - expected) <= tone.segments.count)
        let declared = data[40..<44].withUnsafeBytes { UInt32(littleEndian: $0.loadUnaligned(as: UInt32.self)) }
        #expect(Int(declared) == data.count - 44)
    }

    @Test(arguments: CallTone.allCases) func aToneLoopsWithoutAClick(tone: CallTone) {
        let pcm = samples(tone.waveData())
        // It starts and ends at rest, so the loop point is silent.
        #expect(pcm.first == 0)
        #expect(abs(Int(pcm.last ?? 1)) < 400)
        #expect(pcm.contains { abs(Int($0)) > 2000 })
        // Quiet enough to sit under a conversation.
        #expect(pcm.allSatisfy { abs(Int($0)) <= Int(Double(Int16.max) * CallTone.peak) + 1 })
    }

    @Test func aRingLeavesAPauseBetweenItsPhrases() {
        #expect(CallTone.ring.segments.last?.frequency == nil)
        #expect(CallTone.ringback.segments.last?.frequency == nil)
        let pcm = samples(CallTone.ring.waveData())
        #expect(pcm.suffix(CallTone.sampleRate / 2).allSatisfy { $0 == 0 })
    }
}
