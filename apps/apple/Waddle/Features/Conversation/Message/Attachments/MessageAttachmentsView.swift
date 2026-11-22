import SwiftUI
import WaddleKit

/// XEP-0447 files of a row: inline images, stickers and file cards, with
/// XEP-0448 encrypted files decrypted first.
struct MessageAttachmentsView: View {
    let files: [SharedFile]
    let isSticker: Bool
    let openImage: (SharedFile) -> Void
    /// Opens the pack a sticker is from; nil when it names none.
    var openStickerPack: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: WaddleTokens.Spacing.s1Half) {
            ForEach(files, id: \.url) { file in
                attachment(file)
            }
        }
    }

    @ViewBuilder
    private func attachment(_ file: SharedFile) -> some View {
        let kind = MessageAttachmentKind(file)
        if let source = file.encrypted {
            EncryptedAttachmentView(file: file, source: source, kind: kind, isSticker: isSticker)
        } else {
            plainAttachment(file, kind: kind)
        }
    }

    @ViewBuilder
    private func plainAttachment(_ file: SharedFile, kind: MessageAttachmentKind) -> some View {
        switch kind {
        case .image where isSticker:
            MessageStickerView(file: file, openPack: openStickerPack)
        case .image:
            MessageImageAttachment(file: file, open: openImage)
        default:
            MessageFileCard(file: file, kind: kind)
        }
    }
}

/// An inline image capped to the media width; tap opens an in-app preview.
/// GIFs play; other images load with `AsyncImage`.
struct MessageImageAttachment: View {
    let file: SharedFile
    let open: (SharedFile) -> Void

    static let maxHeight: CGFloat = 320

    private var isGIF: Bool {
        GifMedia.isGIF(mediaType: file.mediaType, url: file.url)
    }

    var body: some View {
        Button {
            open(file)
        } label: {
            image
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(file.description ?? file.displayName))
        .accessibilityAddTraits(.isImage)
        .accessibilityHint(Text("Opens an image preview"))
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
                .font(.system(size: WaddleTokens.FontSize.lg, weight: .semibold))
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

/// Full-screen in-app preview for an image shared in a message.
struct MessageImagePreviewView: View {
    @Environment(\.dismiss) private var dismiss
    let file: SharedFile

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            picture
                .accessibilityLabel(Text(file.description ?? file.displayName))

            VStack {
                HStack {
                    Spacer()
                    Button("Close", systemImage: "xmark") {
                        dismiss()
                    }
                    .labelStyle(.iconOnly)
                    .font(.title2.weight(.semibold))
                    .foregroundStyle(.white)
                    .padding(Theme.Spacing.m)
                    .background(Theme.Colors.scrim, in: Circle())
                }
                Spacer()
            }
            .padding()
        }
        .preferredColorScheme(.dark)
    }

    @ViewBuilder
    private var picture: some View {
        if GifMedia.isGIF(mediaType: file.mediaType, url: file.url) {
            RemoteImageDataView(url: file.url) { phase in
                switch phase {
                case let .success(data):
                    AnimatedImageView(data: data, maxPixelSize: 1600)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                case .failure:
                    failureView
                case .empty:
                    loadingView
                }
            }
        } else {
            AsyncImage(url: file.url) { phase in
                switch phase {
                case let .success(image):
                    image
                        .resizable()
                        .scaledToFit()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                case .failure:
                    failureView
                case .empty:
                    loadingView
                @unknown default:
                    loadingView
                }
            }
        }
    }

    private var failureView: some View {
        ContentUnavailableView("Couldn't Load Image", systemImage: "photo.badge.exclamationmark")
            .foregroundStyle(.white)
    }

    private var loadingView: some View {
        ProgressView()
            .tint(.white)
    }
}
