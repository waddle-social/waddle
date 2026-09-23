import SwiftUI
import WaddleKit

/// A decrypted XEP-0448 image, sized like a plain inline image; tap
/// previews it.
struct DecryptedImageAttachment: View {
    let file: SharedFile
    let image: Image
    let isSticker: Bool
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            sized
                .clipShape(RoundedRectangle(cornerRadius: isSticker ? 0 : Theme.Radius.medium, style: .continuous))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(file.description ?? file.displayName))
        .accessibilityAddTraits(.isImage)
        .accessibilityHint(Text("Opens a preview"))
    }

    @ViewBuilder
    private var sized: some View {
        if isSticker {
            image.resizable().scaledToFit().frame(width: 120, height: 120)
        } else if let size = MessageContent.mediaSize(
            width: file.width,
            height: file.height,
            maxWidth: Double(Theme.Size.mediaMaxWidth),
            maxHeight: Double(MessageImageAttachment.maxHeight)
        ) {
            image.resizable().scaledToFill()
                .frame(width: CGFloat(size.width), height: CGFloat(size.height))
        } else {
            image.resizable().scaledToFit()
                .frame(maxWidth: Theme.Size.mediaMaxWidth, maxHeight: MessageImageAttachment.maxHeight, alignment: .leading)
        }
    }
}

/// A decrypted XEP-0448 file that is not an image; tap previews it.
struct DecryptedFileCard: View {
    let file: SharedFile
    let kind: MessageAttachmentKind
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            MessageFileCardLabel(
                symbol: kind.symbolName,
                title: file.displayName,
                detail: MessageFileCard.detail(for: file, kind: kind),
                trailingSymbol: "lock.open"
            )
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("\(kind.noun), \(file.displayName), encrypted"))
        .accessibilityHint(Text("Opens a preview"))
    }
}
