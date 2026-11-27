import SwiftUI
import WaddleKit

/// Provides the shared container for inline attachment controls and previews.
struct MessageMediaCard<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            content
        }
        .padding(Theme.Spacing.s)
        .frame(maxWidth: Theme.Size.mediaMaxWidth, alignment: .leading)
        .background(shape.fill(Theme.Colors.surface))
        .overlay {
            shape.strokeBorder(Theme.Colors.border, lineWidth: WaddleTokens.BorderWidth.hair)
                .allowsHitTesting(false)
        }
    }
}

/// Shows an inline attachment's name, kind and size.
struct AttachmentMediaCaption: View {
    let file: SharedFile
    let kind: MessageAttachmentKind
    var downloadedSize: Int?

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
            Text(file.displayName.isEmpty ? kind.noun : file.displayName)
                .huddleText(Theme.Typography.label.weight(700))
                .foregroundStyle(Theme.Colors.fg)
                .lineLimit(1)
                .truncationMode(.middle)
            Text(kind.detail(size: file.size ?? downloadedSize))
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
        }
    }
}

/// Toggles the sound of the current inline player.
struct AttachmentMuteButton: View {
    let playback: InlineMediaPlayback

    var body: some View {
        HuddleIconButton(playback.isMuted ? "Unmute" : "Mute", symbol: playback.isMuted ? "speaker.slash" : "speaker.wave.2") {
            playback.setMuted(!playback.isMuted)
        }
        .foregroundStyle(Theme.Colors.fg)
        .disabled(playback.player == nil)
    }
}
