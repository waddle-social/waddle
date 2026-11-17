import AVFoundation
import WaddleKit

/// Plays one call tone on a loop until told to stop.
///
/// The system can stop or refuse playback without telling the player's
/// owner: an app in the background, Siri, a phone call, or an audio
/// session that is not active yet. So while a tone is wanted, the player
/// is checked every second and started again if it has gone quiet.
///
/// It never touches the audio session. On a Mac there is none. On an
/// iPhone or iPad the only tone is ringback: for a call the system's call
/// screen manages the session is the system's, and for one it does not
/// the tone plays under the app's default category, which obeys the
/// silent switch.
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
        guard let player, !player.isPlaying else { return }
        player.play()
    }

    func stop() {
        guard let player else { return }
        watch?.cancel()
        watch = nil
        player.stop()
        self.player = nil
        tone = nil
    }
}
