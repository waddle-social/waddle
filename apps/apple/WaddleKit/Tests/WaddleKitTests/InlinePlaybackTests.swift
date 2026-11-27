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
            var sound = InlinePlaybackSound()
            let pausedForCall = sound.updateCall(phase, isPlaying: true)
            #expect(pausedForCall)
            #expect(sound.isMuted)
            #expect(!sound.configuresPlaybackCategory)
            let pausedAgain = sound.updateCall(phase, isPlaying: true)
            #expect(!pausedAgain)

            var paused = InlinePlaybackSound()
            let pausedWhileIdle = paused.updateCall(phase, isPlaying: false)
            #expect(!pausedWhileIdle)
            #expect(!paused.isMuted)
            paused.preparePlayback(call: phase)
            #expect(paused.isMuted)
        }
        for phase in [CallState.idle, .ended(sid: "call", reason: .rejected)] {
            var sound = InlinePlaybackSound()
            sound.preparePlayback(call: phase)
            #expect(!sound.isMuted)
            #expect(sound.configuresPlaybackCategory)
        }
    }

    @Test func decliningACallRestoresAudiblePlayback() {
        var sound = InlinePlaybackSound()
        sound.preparePlayback(call: .idle)
        let pausedForRing = sound.updateCall(ringingCall(), isPlaying: true)
        #expect(pausedForRing)
        #expect(sound.isMuted)

        let declined = CallState.ended(sid: "call", reason: .rejected)
        var beforeCallUpdate = sound
        let pausedOnDecline = sound.updateCall(declined, isPlaying: false)
        #expect(!pausedOnDecline)
        #expect(!sound.isMuted)
        sound.preparePlayback(call: declined)
        #expect(!sound.isMuted)

        beforeCallUpdate.preparePlayback(call: declined)
        #expect(!beforeCallUpdate.isMuted)
    }

    @Test func unmutingDuringACallSurvivesPauseAndResumeInThatCall() {
        var sound = InlinePlaybackSound()
        sound.preparePlayback(call: ringingCall())
        #expect(sound.isMuted)
        sound.setMuted(false)

        let active = CallState.active(ActiveCall(peer: callPeer, sid: "call", media: callAudio, join: callJoin))
        let pausedOnAnswer = sound.updateCall(active, isPlaying: false)
        #expect(!pausedOnAnswer)
        sound.preparePlayback(call: active)
        #expect(!sound.isMuted)
        #expect(!sound.configuresPlaybackCategory)
    }

    @Test func aNewCallMutesPlaybackAgain() {
        var sound = InlinePlaybackSound()
        sound.preparePlayback(call: ringingCall())
        sound.setMuted(false)

        let next = ringingCall(id: "next")
        let pausedForNextCall = sound.updateCall(next, isPlaying: true)
        #expect(pausedForNextCall)
        #expect(sound.isMuted)
        sound.preparePlayback(call: next)
        #expect(sound.isMuted)
    }

    @Test func aUserMuteSurvivesTheCallEnding() {
        var sound = InlinePlaybackSound()
        sound.preparePlayback(call: ringingCall())
        sound.setMuted(false)
        sound.setMuted(true)

        let pausedOnEnd = sound.updateCall(.idle, isPlaying: false)
        #expect(!pausedOnEnd)
        sound.preparePlayback(call: .idle)
        #expect(sound.isMuted)
    }

    private func ringingCall(id: String = "call") -> CallState {
        .incoming(IncomingCall(from: callPeer, sid: id, media: callAudio))
    }
}
