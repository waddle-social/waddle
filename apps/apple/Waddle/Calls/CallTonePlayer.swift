import AVFoundation
import WaddleKit

/// Plays one call tone on a loop until told to stop.
@MainActor
final class CallTonePlayer {
    private var player: AVAudioPlayer?
    private var tone: CallTone?
    #if os(iOS)
    private var interruptions: NSObjectProtocol?
    #endif

    /// Asks for `tone`. Asking again for the tone already wanted starts it
    /// if it is not sounding: the system can refuse or stop playback (an
    /// app in the background, another call, Siri), and whoever asks again
    /// expects to hear it.
    func play(_ tone: CallTone) {
        if self.tone == tone, let player {
            if !player.isPlaying { player.play() }
            return
        }
        stop()
        #if os(iOS)
        // A ring obeys the silent switch like any other ring. Ringback is
        // the sound of a call the person placed themselves, so it does not.
        // The media engine sets its own category when the call connects.
        try? AVAudioSession.sharedInstance().setCategory(tone == .ring ? .soloAmbient : .playback)
        resumeAfterInterruptions()
        #endif
        guard let player = try? AVAudioPlayer(data: tone.waveData(), fileTypeHint: AVFileType.wav.rawValue) else { return }
        player.numberOfLoops = -1
        player.play()
        self.player = player
        self.tone = tone
    }

    func stop() {
        guard let player else { return }
        player.stop()
        self.player = nil
        tone = nil
        #if os(iOS)
        if let interruptions { NotificationCenter.default.removeObserver(interruptions) }
        interruptions = nil
        // Hands the speaker back to whatever the tone interrupted. A call
        // that follows activates the session again when its media starts.
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
    }

    #if os(iOS)
    private func resumeAfterInterruptions() {
        interruptions = NotificationCenter.default.addObserver(
            forName: AVAudioSession.interruptionNotification, object: nil, queue: .main
        ) { [weak self] note in
            let ended = (note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt) == AVAudioSession.InterruptionType.ended.rawValue
            guard ended else { return }
            Task { @MainActor in
                guard let self, let player = self.player, !player.isPlaying else { return }
                player.play()
            }
        }
    }
    #endif
}
