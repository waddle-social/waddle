import SwiftUI
import AVKit
import PDFKit

struct ChatMessageAttachmentsView: View {
    let message: ChatTimelineMessage
    let maxWidth: CGFloat
    @State private var lightboxImage: XMPPSharedFile?

    var body: some View {
        Group {
            inlineImagesView(for: message, maxWidth: maxWidth)
            inlineVideosView(for: message, maxWidth: maxWidth)
            inlineAudioFilesView(for: message, maxWidth: maxWidth)
            inlinePdfFilesView(for: message, maxWidth: maxWidth)
            bodyImageURLsView(for: message, maxWidth: maxWidth)
            downloadableFilesView(for: message)
        }
    }

    @ViewBuilder
    private func inlineImagesView(for message: ChatTimelineMessage, maxWidth: CGFloat) -> some View {
        let images = message.inlineImages
        if !images.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(images, id: \.url) { file in
                    Button {
                        lightboxImage = file
                    } label: {
                        AsyncImage(url: URL(string: file.url)) { phase in
                            switch phase {
                            case .success(let image):
                                image
                                    .resizable()
                                    .aspectRatio(contentMode: .fit)
                                    .frame(maxWidth: maxWidth, maxHeight: 240)
                                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                            case .failure:
                                Label(file.name ?? "Image", systemImage: "photo")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .padding(8)
                                    .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
                            case .empty:
                                RoundedRectangle(cornerRadius: 12, style: .continuous)
                                    .fill(Color.secondary.opacity(0.08))
                                    .frame(width: min(CGFloat(file.width ?? 200), maxWidth), height: min(CGFloat(file.height ?? 150), 240))
                                    .overlay { ProgressView() }
                            @unknown default:
                                EmptyView()
                            }
                        }
                    }
                    .buttonStyle(.plain)
                }
            }
#if os(iOS)
            .fullScreenCover(item: $lightboxImage) { file in
                ChatImageLightboxView(file: file)
            }
#else
            .sheet(item: $lightboxImage) { file in
                ChatImageLightboxView(file: file)
                    .frame(minWidth: 600, minHeight: 500)
            }
#endif
        }
    }

    @ViewBuilder
    private func downloadableFilesView(for message: ChatTimelineMessage) -> some View {
        let files = message.downloadableFiles
        if !files.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(files, id: \.url) { file in
                    if let url = URL(string: file.url) {
                        Link(destination: url) {
                            HStack(spacing: 8) {
                                Image(systemName: "arrow.down.circle")
                                    .font(.subheadline)
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(file.name ?? "File")
                                        .font(.caption.weight(.medium))
                                        .lineLimit(1)
                                    HStack(spacing: 4) {
                                        Text(file.mediaType ?? "file")
                                            .font(.caption2)
                                        if let size = file.size {
                                            Text("·")
                                            Text(formatFileSize(size))
                                                .font(.caption2)
                                        }
                                    }
                                    .foregroundStyle(.secondary)
                                }
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 8)
                            .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func inlineVideosView(for message: ChatTimelineMessage, maxWidth: CGFloat) -> some View {
        let files = message.inlineVideos
        if !files.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(files, id: \.url) { file in
                    if let url = URL(string: file.url) {
                        ChatMediaPlayerAttachmentView(
                            url: url,
                            fileName: file.name ?? "Video",
                            mediaType: file.mediaType,
                            size: file.size,
                            height: 220
                        )
                        .frame(maxWidth: maxWidth)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func inlineAudioFilesView(for message: ChatTimelineMessage, maxWidth: CGFloat) -> some View {
        let files = message.inlineAudioFiles
        if !files.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(files, id: \.url) { file in
                    if let url = URL(string: file.url) {
                        ChatMediaPlayerAttachmentView(
                            url: url,
                            fileName: file.name ?? "Audio",
                            mediaType: file.mediaType,
                            size: file.size,
                            height: 84
                        )
                        .frame(maxWidth: maxWidth)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func inlinePdfFilesView(for message: ChatTimelineMessage, maxWidth: CGFloat) -> some View {
        let files = message.inlinePdfFiles
        if !files.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(files, id: \.url) { file in
                    if let url = URL(string: file.url) {
                        ChatPdfAttachmentView(
                            url: url,
                            fileName: file.name ?? "PDF",
                            mediaType: file.mediaType,
                            size: file.size
                        )
                        .frame(maxWidth: maxWidth, minHeight: 220, maxHeight: 220)
                    }
                }
            }
        }
    }

    /// Render each URL in the message body that resolves to an image/GIF as
    /// an inline preview. Complements the XEP-0385 `sharedFiles` path for
    /// the common case where a sender just pasted a Tenor/Giphy/CDN link
    /// rather than attaching via file-sharing.
    @ViewBuilder
    private func bodyImageURLsView(for message: ChatTimelineMessage, maxWidth: CGFloat) -> some View {
        let urls = message.detectedImageURLs
        if !urls.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(urls, id: \.absoluteString) { url in
                    AsyncImage(url: url) { phase in
                        switch phase {
                        case .success(let image):
                            image
                                .resizable()
                                .aspectRatio(contentMode: .fit)
                                .frame(maxWidth: maxWidth, maxHeight: 240)
                                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                                .overlay(
                                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                                        .strokeBorder(WaddleTheme.divider, lineWidth: 0.5)
                                )
                        case .failure:
                            Label(url.lastPathComponent.isEmpty ? "Image" : url.lastPathComponent, systemImage: "photo")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .padding(8)
                                .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
                        case .empty:
                            RoundedRectangle(cornerRadius: 12, style: .continuous)
                                .fill(Color.secondary.opacity(0.08))
                                .frame(width: 180, height: 120)
                                .overlay(ProgressView())
                        @unknown default:
                            EmptyView()
                        }
                    }
                }
            }
        }
    }


}

private struct ChatMediaPlayerAttachmentView: View {
    let url: URL
    let fileName: String
    let mediaType: String?
    let size: Int?
    let height: CGFloat

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            VideoPlayer(player: AVPlayer(url: url))
                .frame(maxWidth: .infinity, minHeight: height, maxHeight: height)
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .strokeBorder(WaddleTheme.divider, lineWidth: 0.5)
                )
            attachmentMeta
        }
    }

    private var attachmentMeta: some View {
        HStack(spacing: 8) {
            Image(systemName: mediaType?.hasPrefix("audio/") == true ? "waveform" : "film")
                .font(.caption.weight(.semibold))
                .foregroundStyle(WaddleTheme.textSecondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(fileName)
                    .font(.caption.weight(.semibold))
                    .lineLimit(1)
                HStack(spacing: 4) {
                    Text(mediaType ?? "media")
                        .font(.caption2)
                    if let size {
                        Text("·")
                        Text(formatFileSize(size))
                            .font(.caption2)
                    }
                }
                .foregroundStyle(WaddleTheme.textSecondary)
            }
            Spacer()
        }
        .padding(.horizontal, 10)
    }
}

private struct ChatPdfAttachmentView: View {
    let url: URL
    let fileName: String
    let mediaType: String?
    let size: Int?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ChatPdfPreview(url: url)
                .frame(maxWidth: .infinity, minHeight: 220, maxHeight: 220)
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .strokeBorder(WaddleTheme.divider, lineWidth: 0.5)
                )
            HStack(spacing: 8) {
                Image(systemName: "doc.richtext")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(WaddleTheme.textSecondary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(fileName)
                        .font(.caption.weight(.semibold))
                        .lineLimit(1)
                    HStack(spacing: 4) {
                        Text(mediaType ?? "application/pdf")
                            .font(.caption2)
                        if let size {
                            Text("·")
                            Text(formatFileSize(size))
                                .font(.caption2)
                        }
                    }
                    .foregroundStyle(WaddleTheme.textSecondary)
                }
                Spacer()
            }
            .padding(.horizontal, 10)
        }
    }
}

#if os(iOS)
private struct ChatPdfPreview: UIViewRepresentable {
    let url: URL

    func makeUIView(context: Context) -> PDFView {
        let view = PDFView()
        view.autoScales = true
        view.displayMode = .singlePageContinuous
        view.displayDirection = .vertical
        view.document = PDFDocument(url: url)
        return view
    }

    func updateUIView(_ uiView: PDFView, context: Context) {
        if uiView.document == nil {
            uiView.document = PDFDocument(url: url)
        }
    }
}
#elseif os(macOS)
private struct ChatPdfPreview: NSViewRepresentable {
    let url: URL

    func makeNSView(context: Context) -> PDFView {
        let view = PDFView()
        view.autoScales = true
        view.displayMode = .singlePageContinuous
        view.displayDirection = .vertical
        view.document = PDFDocument(url: url)
        return view
    }

    func updateNSView(_ nsView: PDFView, context: Context) {
        if nsView.document == nil {
            nsView.document = PDFDocument(url: url)
        }
    }
}
#endif

private func formatFileSize(_ bytes: Int) -> String {
    if bytes < 1024 { return "\(bytes) B" }
    if bytes < 1024 * 1024 { return "\(bytes / 1024) KB" }
    return String(format: "%.1f MB", Double(bytes) / (1024 * 1024))
}

