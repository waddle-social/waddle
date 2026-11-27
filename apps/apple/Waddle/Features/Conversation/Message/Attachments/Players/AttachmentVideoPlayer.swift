import SwiftUI
import WaddleKit

/// Shows an attachment's video with the system player's controls.
struct AttachmentVideoPlayer: View {
    let file: SharedFile
    let playback: InlineMediaPlayback
    let togglePlayback: () -> Void

    var body: some View {
        MessageMediaCard {
            ZStack {
                Theme.Colors.surface2
                if let player = playback.player {
                    NativeAttachmentVideo(player: player)
                } else {
                    VStack(spacing: Theme.Spacing.xs) {
                        HuddleIconButton(playLabel, symbol: playback.didFail ? "arrow.clockwise" : "play.fill",
                                         side: Theme.Size.minHitTarget, action: togglePlayback)
                            .foregroundStyle(Theme.Material.mediaFg)
                            .background(Theme.Material.media, in: Circle())
                        if playback.didFail {
                            Text("Couldn't play video")
                                .huddleText(Theme.Typography.caption)
                                .foregroundStyle(Theme.Colors.fgMuted)
                        }
                    }
                }
            }
            .aspectRatio(aspectRatio, contentMode: .fit)
            .frame(maxHeight: 288)
            .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous))
            HStack {
                AttachmentMediaCaption(file: file, kind: .video)
                    .accessibilityElement(children: .combine)
                    .accessibilityAction(named: Text(playback.isPlaying ? "Pause video" : playLabel), togglePlayback)
                Spacer(minLength: 0)
                if playback.player != nil {
                    AttachmentMuteButton(playback: playback)
                }
            }
            if playback.didFail {
                Link("Open attachment", destination: file.url)
            }
        }
    }

    private var playLabel: String {
        if playback.didFail { return "Retry video" }
        return playback.elapsed > 0 ? "Resume video" : "Play video"
    }

    private var aspectRatio: CGFloat {
        guard let width = file.width, let height = file.height, width > 0, height > 0 else { return 16 / 9 }
        return CGFloat(width) / CGFloat(height)
    }
}
