import SwiftUI
import WaddleKit

struct MessageLinkPreviewCard: View {
    @Environment(\.openURL) private var openURL
    @State private var state: LinkPreviewCardState
    let preview: LinkPreview
    let opensLink: Bool
    let startMuted: () -> Bool

    init(preview: LinkPreview, opensLink: Bool = true, startMuted: @escaping () -> Bool = { false }) {
        self.preview = preview
        self.opensLink = opensLink
        self.startMuted = startMuted
        _state = State(initialValue: LinkPreviewCardState(playback: opensLink ? preview.playback : nil))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            switch state {
            case .link, .fallback:
                if opensLink {
                    Button { openURL(preview.url) } label: { staticContent }
                        .buttonStyle(.plain)
                        .accessibilityLabel(Text(preview.title ?? preview.url.absoluteString))
                        .accessibilityAddTraits(.isLink)
                } else {
                    staticContent
                }
            case .ready, .video, .embed:
                card {
                    Button { openURL(preview.url) } label: { details }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(.isLink)
                    playbackSurface
                        .padding(.top, Theme.Spacing.xs)
                }
            }
        }
        .frame(maxWidth: Theme.Size.mediaMaxWidth + Theme.Spacing.m * 2, alignment: .leading)
        .onDisappear { state.stop(playback: preview.playback) }
    }

    private var staticContent: some View {
        card {
            details
            if let imageURL = preview.imageURL {
                previewImage(imageURL)
                    .frame(maxWidth: .infinity, minHeight: 120, maxHeight: 160)
                    .clipShape(RoundedRectangle(cornerRadius: WaddleTokens.Radius.sm, style: .continuous))
                    .accessibilityLabel(Text(preview.imageAlt ?? ""))
                    .padding(.top, Theme.Spacing.xs)
            }
        }
    }

    private func card(@ViewBuilder content: () -> some View) -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
        return VStack(alignment: .leading, spacing: Theme.Spacing.xs, content: content)
            .multilineTextAlignment(.leading)
            .fixedSize(horizontal: false, vertical: true)
            .padding(Theme.Spacing.m)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(shape.fill(Theme.Colors.surface))
            .overlay {
                shape.strokeBorder(Theme.Colors.border, lineWidth: WaddleTokens.BorderWidth.hair)
                    .allowsHitTesting(false)
            }
            .contentShape(shape)
    }

    private var details: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            Label {
                Text(preview.url.host() ?? preview.url.absoluteString)
            } icon: {
                Image(systemName: "arrow.up.forward.square")
            }
            .huddleText(Theme.Typography.caption)
            .foregroundStyle(Theme.Colors.fgMuted)
            .lineLimit(1)
            if let title = preview.title, !title.isEmpty {
                Text(title)
                    .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.md).weight(700))
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(2)
            }
            if let summary = preview.summary, !summary.isEmpty {
                Text(summary)
                    .huddleText(Theme.Typography.caption.sized(WaddleTokens.FontSize.sm))
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(3)
            }
        }
    }

    private var playbackSurface: some View {
        Theme.Colors.surface2
            .aspectRatio(CGFloat(playbackAspectRatio), contentMode: .fit)
            .overlay {
                switch state {
                case .ready:
                    Button { state.play(muted: startMuted()) } label: {
                        ZStack {
                            if let imageURL = preview.imageURL { previewImage(imageURL) }
                            Image(systemName: "play.fill")
                                .font(.system(size: WaddleTokens.FontSize.lg, weight: .semibold))
                                .foregroundStyle(Theme.Colors.fg)
                                .frame(width: WaddleTokens.ControlHeight.lg, height: WaddleTokens.ControlHeight.lg)
                                .huddleSurface(.raised, in: Circle())
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(Text(preview.title.map { "Play video: \($0)" } ?? "Play video"))
                case let .video(video, muted):
                    LinkPreviewAVPlayer(video: video, muted: muted) { state.fail() }
                case let .embed(embed):
                    LinkPreviewWebPlayer(embed: embed, onFailure: { state.fail() })
                case .link, .fallback:
                    EmptyView()
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.small))
    }

    private var playbackAspectRatio: Double {
        switch state {
        case let .ready(.embed(embed)), let .embed(embed): embed.aspectRatio
        default: LinkPreviewEmbed.defaultAspectRatio
        }
    }

    private func previewImage(_ url: URL) -> some View {
        AsyncImage(url: url) { image in
            image.resizable().scaledToFill()
        } placeholder: {
            Theme.Colors.surface2
        }
    }
}
