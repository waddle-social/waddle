import Testing
import WaddleKit

struct InlinePlaybackTests {
    @Test func transfersTheSinglePlaybackSlotAndIgnoresStaleDismissals() {
        var selection = InlinePlaybackSelection<String>()
        #expect(selection.start("audio") == nil)
        #expect(selection.current == "audio")
        #expect(selection.start("audio") == nil)

        #expect(selection.start("video") == "audio")
        selection.stop("audio")
        #expect(selection.current == "video")

        #expect(selection.start("audio") == "video")
        selection.stop("audio")
        #expect(selection.current == nil)
        #expect(selection.start("video") == nil)
    }

    @Test func protectsTheAudioSessionInEveryLiveCallPhase() {
        let room = callPeer.bare
        let phases: [CallState] = [
            .incoming(IncomingCall(from: callPeer, sid: "call", media: callAudio)),
            .incoming(IncomingCall(from: callPeer, sid: "call", media: callAudio, isAccepting: true)),
            .outgoing(OutgoingCall(to: callPeer.bare, sid: "call", media: callAudio)),
            .outgoing(OutgoingCall(to: callPeer.bare, sid: "call", media: callAudio, isRinging: true)),
            .active(ActiveCall(peer: callPeer, sid: "call", media: callAudio, join: callJoin)),
            .joiningGroup(GroupCallAttempt(room: room, sid: "group", media: callAudio, nick: "me")),
            .inGroup(GroupCall(room: room, sid: "group", media: callAudio, join: callJoin, nick: "me")),
        ]
        for phase in phases {
            let sound = InlinePlaybackSound(call: phase)
            #expect(sound.startsMuted)
            #expect(!sound.configuresPlaybackCategory)
            #expect(sound.pausesPlayback(wasCallLive: false, isPlaying: true))
            #expect(!sound.pausesPlayback(wasCallLive: false, isPlaying: false))
            #expect(!sound.pausesPlayback(wasCallLive: true, isPlaying: true))
        }
        for phase in [CallState.idle, .ended(sid: "call", reason: .rejected)] {
            let sound = InlinePlaybackSound(call: phase)
            #expect(!sound.startsMuted)
            #expect(sound.configuresPlaybackCategory)
            #expect(!sound.pausesPlayback(wasCallLive: true, isPlaying: true))
        }
    }
}
