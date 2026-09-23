import SwiftUI
import WaddleKit

/// Pending attachments with upload progress, retry on failure, and remove.
struct ComposerAttachmentStrip: View {
    let model: ComposerModel
    let uploader: AttachmentUploader

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Theme.Spacing.s) {
                ForEach(model.attachments) { attachment in
                    ComposerAttachmentChip(
                        attachment: attachment,
                        onRetry: { model.retryAttachment(attachment.id, uploader: uploader) },
                        onRemove: { model.removeAttachment(attachment.id) }
                    )
                }
            }
            .padding(.top, Theme.Spacing.xs)
            .padding(.horizontal, Theme.Spacing.xxs)
        }
    }
}

private struct ComposerAttachmentChip: View {
    let attachment: ComposerAttachment
    let onRetry: () -> Void
    let onRemove: () -> Void

    var body: some View {
        preview
            .overlay { status }
            .overlay(alignment: .topTrailing) {
                Button(action: onRemove) {
                    Image(systemName: "xmark.circle.fill")
                        .symbolRenderingMode(.palette)
                        .foregroundStyle(Color.white, Color.black.opacity(0.6))
                        .font(.body)
                }
                .buttonStyle(.plain)
                .offset(x: 6, y: -6)
                .accessibilityLabel(Text("Remove \(attachment.filename)"))
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(Text(accessibilityText))
    }

    @ViewBuilder
    private var preview: some View {
        if let data = attachment.thumbnail, let image = Image(data: data) {
            image
                .resizable()
                .scaledToFill()
                .frame(width: 64, height: 64)
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
        } else {
            MessageFileCardLabel(
                symbol: attachment.isImage ? "photo" : "doc",
                title: attachment.filename,
                detail: ByteCountFormatter.string(fromByteCount: Int64(attachment.byteCount), countStyle: .file),
                trailingSymbol: "paperclip"
            )
            .frame(width: 220)
        }
    }

    @ViewBuilder
    private var status: some View {
        switch attachment.phase {
        case let .uploading(fraction):
            ZStack {
                RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                    .fill(Color.black.opacity(0.35))
                ProgressView(value: fraction)
                    .progressViewStyle(.circular)
                    .tint(Color.white)
            }
        case .uploaded:
            EmptyView()
        case let .failed(message):
            ZStack {
                RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
                    .fill(Color.black.opacity(0.45))
                Button(action: onRetry) {
                    Label("Retry", systemImage: "arrow.clockwise")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Color.white)
                }
                .buttonStyle(.plain)
                .help(message)
            }
        }
    }

    private var accessibilityText: String {
        switch attachment.phase {
        case .uploading: return "\(attachment.filename), uploading"
        case .uploaded: return "\(attachment.filename), ready to send"
        case let .failed(message): return "\(attachment.filename), upload failed. \(message)"
        }
    }
}
