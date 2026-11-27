import SwiftUI
import WaddleKit

/// Picks the attachment's inline controls and keeps playback scoped to its row.
struct InlineMediaPlayer: View {
    enum Medium { case audio, video }

    @Environment(SessionCoordinator.self) private var session
    @State private var playback = InlineMediaPlayback()
    let file: SharedFile
    let medium: Medium

    var body: some View {
        let player = playback.player
        Group {
            switch medium {
            case .audio:
                AttachmentAudioPlayer(file: file, playback: playback, togglePlayback: togglePlayback)
            case .video:
                AttachmentVideoPlayer(file: file, playback: playback, togglePlayback: togglePlayback)
            }
        }
        .task(id: player) {
            if let player { await playback.observe(player) }
        }
        .onDisappear { playback.stop() }
        .onChange(of: session.calls.state.isLive) { wasLive, _ in
            playback.updateCall(session.calls.state, wasLive: wasLive)
        }
    }

    private func togglePlayback() {
        playback.togglePlayback(url: file.inlineMediaURL) { [session] in session.calls.state }
    }
}
