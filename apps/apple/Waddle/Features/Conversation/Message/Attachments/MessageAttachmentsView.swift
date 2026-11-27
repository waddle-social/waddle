import SwiftUI
import WaddleKit

/// XEP-0447 files of a row: inline images, stickers and file cards, with
/// XEP-0448 encrypted files decrypted first.
struct MessageAttachmentsView: View {
    let attachments: [MessageMedia.Attachment]
    let isSticker: Bool
    let openImage: (MediaGallery.Source) -> Void
    /// Opens the pack a sticker is from; nil when it names none.
    var openStickerPack: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: WaddleTokens.Spacing.s1Half) {
            ForEach(attachments) { attachment in
                content(attachment)
            }
        }
    }

    @ViewBuilder
    private func content(_ attachment: MessageMedia.Attachment) -> some View {
        let file = attachment.file
        let kind = MessageAttachmentKind(file)
        if let source = file.encrypted {
            EncryptedAttachmentView(file: file, source: source, kind: kind, isSticker: isSticker)
        } else {
            switch kind {
            case .image where isSticker:
                MessageStickerView(file: file, openPack: openStickerPack)
            case .image:
                MessageImageAttachment(file: file) { openImage(.attachment(attachment.id)) }
            default:
                MessageFileCard(file: file, kind: kind)
            }
        }
    }
}

/// An inline image capped to the media width; tap opens the gallery.
/// GIFs play; other images load with `AsyncImage`.
struct MessageImageAttachment: View {
    let file: SharedFile
    let open: () -> Void

    static let maxHeight: CGFloat = 320

    private var isGIF: Bool {
        GifMedia.isGIF(mediaType: file.mediaType, url: file.url)
    }

    var body: some View {
        Button(action: open) {
            image
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(file.description ?? file.displayName))
        .accessibilityAddTraits(.isImage)
        .accessibilityHint(Text("Opens the image gallery"))
    }

    @ViewBuilder
    private var image: some View {
        if let size = MessageContent.mediaSize(
            width: file.width,
            height: file.height,
            maxWidth: Double(Theme.Size.mediaMaxWidth),
            maxHeight: Double(Self.maxHeight)
        ) {
            picture(fill: true)
                .frame(width: CGFloat(size.width), height: CGFloat(size.height))
        } else {
            picture(fill: false)
                .frame(maxWidth: Theme.Size.mediaMaxWidth, maxHeight: Self.maxHeight, alignment: .leading)
        }
    }

    @ViewBuilder
    private func picture(fill: Bool) -> some View {
        if isGIF {
            RemoteImageDataView(url: file.url) { phase in
                gifPhaseView(phase)
            }
        } else {
            AsyncImage(url: file.url) { phase in
                phaseView(phase, fill: fill)
            }
        }
    }

    @ViewBuilder
    private func gifPhaseView(_ phase: RemoteImageDataPhase) -> some View {
        switch phase {
        case let .success(data):
            AnimatedImageView(data: data)
        case .failure:
            placeholder(symbol: "photo.badge.exclamationmark")
        case .empty:
            placeholder(symbol: nil)
        }
    }

    @ViewBuilder
    private func phaseView(_ phase: AsyncImagePhase, fill: Bool) -> some View {
        switch phase {
        case let .success(image):
            if fill {
                image.resizable().scaledToFill()
            } else {
                image.resizable().scaledToFit()
            }
        case .failure:
            placeholder(symbol: "photo.badge.exclamationmark")
        case .empty:
            placeholder(symbol: nil)
        @unknown default:
            placeholder(symbol: nil)
        }
    }

    private func placeholder(symbol: String?) -> some View {
        ZStack {
            Theme.Colors.surface2
            if let symbol {
                Image(systemName: symbol)
                    .font(.title2)
                    .foregroundStyle(Theme.Colors.fgMuted)
            } else {
                ProgressView()
            }
        }
        .frame(minWidth: 160, minHeight: 120)
    }
}

/// A sticker: the image alone, without chrome. GIF stickers play. The
/// row hides a sticker's text, so a picture that cannot be fetched is
/// replaced by that text.
struct MessageStickerView: View {
    let file: SharedFile
    /// Opens the pack the sticker is from; nil when it names none.
    var openPack: (() -> Void)?

    var body: some View {
        tappable
            .accessibilityLabel(Text(file.description ?? "Sticker"))
            .accessibilityAddTraits(.isImage)
    }

    @ViewBuilder
    private var tappable: some View {
        if let openPack {
            Button(action: openPack) { sticker }
                .buttonStyle(.plain)
                .accessibilityHint(Text("Shows the pack this sticker is from"))
        } else {
            sticker
        }
    }

    @ViewBuilder
    private var sticker: some View {
        if GifMedia.isGIF(mediaType: file.mediaType, url: file.url) {
            RemoteImageDataView(url: file.url) { phase in
                switch phase {
                case let .success(data):
                    AnimatedImageView(data: data, maxPixelSize: 360).frame(width: 120, height: 120)
                case .empty:
                    Color.clear.frame(width: 120, height: 120)
                case .failure:
                    unavailable
                }
            }
        } else {
            AsyncImage(url: file.url) { phase in
                switch phase {
                case let .success(image):
                    image.resizable().scaledToFit().frame(width: 120, height: 120)
                case .empty:
                    Color.clear.frame(width: 120, height: 120)
                case .failure:
                    unavailable
                @unknown default:
                    unavailable
                }
            }
        }
    }

    /// What the sticker shows, in words, where its picture would be.
    private var unavailable: some View {
        Label(file.description ?? "Sticker", systemImage: "face.smiling")
            .huddleText(Theme.Typography.body)
            .foregroundStyle(Theme.Colors.fgMuted)
    }
}

/// Video, audio, PDF and other files: icon, name, size; tap opens.
struct MessageFileCard: View {
    @Environment(\.openURL) private var openURL
    let file: SharedFile
    let kind: MessageAttachmentKind

    var body: some View {
        Button {
            openURL(file.url)
        } label: {
            MessageFileCardLabel(
                symbol: kind.symbolName,
                title: file.displayName,
                detail: detail,
                trailingSymbol: "arrow.down.circle"
            )
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("\(kind.noun), \(file.displayName)"))
    }

    private var detail: String {
        Self.detail(for: file, kind: kind)
    }

    /// "PDF · 1.2 MB", or the kind alone when the size is unknown.
    static func detail(for file: SharedFile, kind: MessageAttachmentKind) -> String {
        guard let size = file.size else { return kind.noun }
        return "\(kind.noun) · \(ByteCountFormatter.string(fromByteCount: Int64(size), countStyle: .file))"
    }
}

/// Shared card layout for file attachments.
struct MessageFileCardLabel: View {
    let symbol: String
    let title: String
    let detail: String
    let trailingSymbol: String

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            Image(systemName: symbol)
                .huddleText(Theme.Typography.subtitle)
                .foregroundStyle(Theme.Colors.actionText)
                .frame(width: WaddleTokens.ControlHeight.md, height: WaddleTokens.ControlHeight.md)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous)
                        .fill(Theme.Colors.actionSubtle)
                )
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(title)
                    .huddleText(Theme.Typography.label.weight(700))
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(detail)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(1)
            }
            Spacer(minLength: Theme.Spacing.s)
            Image(systemName: trailingSymbol)
                .foregroundStyle(Theme.Colors.fgMuted)
                .accessibilityHidden(true)
        }
        .padding(Theme.Spacing.m)
        .frame(maxWidth: Theme.Size.mediaMaxWidth, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                .fill(Theme.Colors.surface)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                .strokeBorder(Theme.Colors.border, lineWidth: WaddleTokens.BorderWidth.hair)
        )
        .contentShape(Rectangle())
    }
}
