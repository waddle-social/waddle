import SwiftUI
import WaddleKit

/// XEP-0447 files of a row: inline images, stickers and file cards, with
/// XEP-0448 encrypted files decrypted first.
struct MessageAttachmentsView: View {
    let files: [SharedFile]
    let isSticker: Bool
    let openImage: (SharedFile) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s - 2) {
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
            MessageStickerView(file: file)
        case .image:
            MessageImageAttachment(file: file, open: openImage)
        default:
            MessageFileCard(file: file, kind: kind)
        }
    }
}

/// An inline image capped to the media width; tap opens an in-app preview.
struct MessageImageAttachment: View {
    let file: SharedFile
    let open: (SharedFile) -> Void

    static let maxHeight: CGFloat = 320

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
            AsyncImage(url: file.url) { phase in
                phaseView(phase, fill: true)
            }
            .frame(width: CGFloat(size.width), height: CGFloat(size.height))
        } else {
            AsyncImage(url: file.url) { phase in
                phaseView(phase, fill: false)
            }
            .frame(maxWidth: Theme.Size.mediaMaxWidth, maxHeight: Self.maxHeight, alignment: .leading)
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
            Color.secondary.opacity(0.12)
            if let symbol {
                Image(systemName: symbol)
                    .font(.title2)
                    .foregroundStyle(.secondary)
            } else {
                ProgressView()
            }
        }
        .frame(minWidth: 160, minHeight: 120)
    }
}

/// A sticker: the image alone, without chrome.
struct MessageStickerView: View {
    let file: SharedFile

    var body: some View {
        AsyncImage(url: file.url) { image in
            image.resizable().scaledToFit()
        } placeholder: {
            Color.clear
        }
        .frame(width: 120, height: 120)
        .accessibilityLabel(Text(file.description ?? "Sticker"))
        .accessibilityAddTraits(.isImage)
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
                .font(.title3)
                .foregroundStyle(Color.accentColor)
                .frame(width: 36, height: 36)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous)
                        .fill(Color.accentColor.opacity(0.12))
                )
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.subheadline.weight(.medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: Theme.Spacing.s)
            Image(systemName: trailingSymbol)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
        }
        .padding(Theme.Spacing.s + 2)
        .frame(maxWidth: Theme.Size.mediaMaxWidth, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                .fill(Color.secondaryBackground)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                .strokeBorder(Color.secondary.opacity(0.15), lineWidth: 1)
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

            AsyncImage(url: file.url) { phase in
                switch phase {
                case let .success(image):
                    image
                        .resizable()
                        .scaledToFit()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                case .failure:
                    ContentUnavailableView("Couldn't Load Image", systemImage: "photo.badge.exclamationmark")
                        .foregroundStyle(.white)
                case .empty:
                    ProgressView()
                        .tint(.white)
                @unknown default:
                    ProgressView()
                        .tint(.white)
                }
            }
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
                    .padding(12)
                    .background(.black.opacity(0.55), in: Circle())
                }
                Spacer()
            }
            .padding()
        }
        .preferredColorScheme(.dark)
    }
}
