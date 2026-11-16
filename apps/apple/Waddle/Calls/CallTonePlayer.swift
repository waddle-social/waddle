import AVFoundation
import WaddleKit

/// Plays one call tone on a loop until told to stop.
@MainActor
final class CallTonePlayer {
    private var player: AVAudioPlayer?
    private(set) var playing: CallTone?

    func play(_ tone: CallTone) {
        guard playing != tone else { return }
        stop()
        #if os(iOS)
        // A ring obeys the silent switch like any other ring. Ringback is
        // the sound of a call the person placed themselves, so it does not.
        // The media engine sets its own category when the call connects.
        try? AVAudioSession.sharedInstance().setCategory(tone == .ring ? .soloAmbient : .playback)
        #endif
        guard let player = try? AVAudioPlayer(data: tone.waveData(), fileTypeHint: AVFileType.wav.rawValue) else { return }
        player.numberOfLoops = -1
        player.play()
        self.player = player
        playing = tone
    }

    func stop() {
        player?.stop()
        player = nil
        playing = nil
    }
}
