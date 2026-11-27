import SwiftUI
import WaddleKit

/// Names a message's withheld media so the reader can decide whether to load it.
struct WithheldMediaCard: View {
    let media: MessageMedia.Withheld
    let load: () -> Void

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            Text("Images and files from \(media.host?.description ?? "an unknown server") aren't loaded until you choose to.")
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.fgMuted)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                ForEach(media.items.indices, id: \.self) { index in
                    Text(title(for: media.items[index]))
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .lineLimit(1)
                }
            }
            Button("Load media", action: load)
                .buttonStyle(.huddle(.secondary, size: .small))
        }
        .padding(Theme.Spacing.m)
        .frame(maxWidth: Theme.Size.mediaMaxWidth, alignment: .leading)
        .background(shape.fill(Theme.Colors.surface))
        .overlay {
            shape.strokeBorder(Theme.Colors.border, style: StrokeStyle(lineWidth: WaddleTokens.BorderWidth.hair, dash: [4, 3]))
                .allowsHitTesting(false)
        }
    }

    private func title(for item: MessageMedia.Withheld.Item) -> String {
        switch item {
        case let .attachment(name), let .inlineImage(name): name
        case .linkPreviewImages: "Link preview pictures"
        }
    }
}
