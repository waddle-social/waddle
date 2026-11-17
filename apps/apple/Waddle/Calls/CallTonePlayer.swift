import AVFoundation
import WaddleKit

/// Plays one call tone on a loop until told to stop.
///
/// The system can stop or refuse playback without telling the player's
/// owner: an app in the background, Siri, a phone call, or the media
/// engine of a call that just ended releasing the audio session. So while
/// a tone is wanted, the player is checked every second and started again
/// if it has gone quiet.
@MainActor
final class CallTonePlayer {
    private var player: AVAudioPlayer?
    private var tone: CallTone?
    private var watch: Task<Void, Never>?

    static let watchInterval: Duration = .seconds(1)

    func play(_ tone: CallTone) {
        if self.tone == tone, player != nil {
            startIfQuiet()
            return
        }
        stop()
        guard let player = try? AVAudioPlayer(data: tone.waveData(), fileTypeHint: AVFileType.wav.rawValue) else { return }
        player.numberOfLoops = -1
        self.player = player
        self.tone = tone
        startIfQuiet()
        watch = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: Self.watchInterval)
                guard !Task.isCancelled, let self else { return }
                self.startIfQuiet()
            }
        }
    }

    private func startIfQuiet() {
        guard let player, let tone, !player.isPlaying else { return }
        #if os(iOS)
        // A ring obeys the silent switch like any other ring. Ringback is
        // the sound of a call the person placed themselves, so it does not.
        // Set before every start: a call that just ended can leave its own
        // category behind, and the media engine sets its own again when
        // the next call connects.
        try? AVAudioSession.sharedInstance().setCategory(tone == .ring ? .soloAmbient : .playback)
        #endif
        player.play()
    }

    func stop() {
        guard let player else { return }
        watch?.cancel()
        watch = nil
        player.stop()
        self.player = nil
        tone = nil
        #if os(iOS)
        // Hands the speaker back to whatever the tone interrupted. A call
        // that follows activates the session again when its media starts.
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
    }
}
