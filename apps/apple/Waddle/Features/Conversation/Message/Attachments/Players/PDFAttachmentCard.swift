import QuickLook
import SwiftUI
import WaddleKit

/// Loads an attachment preview and presents the cached PDF in Quick Look.
struct PDFAttachmentView: View {
    @Environment(\.openURL) private var openURL
    let file: SharedFile
    @State private var preview: PDFAttachmentPreview?
    @State private var presented: PDFAttachmentPreview?
    @State private var failure: PDFAttachmentCard.Phase?

    var body: some View {
        let phase = preview.map { PDFAttachmentCard.Phase.loaded(Image(decorative: $0.thumbnail, scale: 1), size: $0.size) }
        PDFAttachmentCard(file: file, phase: phase ?? failure ?? .loading, open: open)
            .task { await load() }
            .quickLookPreview(previewURL)
            .onDisappear { preview = nil }
    }

    // Quick Look keeps its copy alive even if the row scrolls away.
    private var previewURL: Binding<URL?> {
        Binding(get: { presented?.url }, set: { if $0 == nil { presented = nil } })
    }

    private func load() async {
        guard preview == nil, failure == nil else { return }
        do {
            let loaded = try await PDFAttachmentStore.shared.load(file)
            try Task.checkCancellation()
            preview = loaded
        } catch RemoteMediaBytes.LoadError.tooLarge {
            if !Task.isCancelled { failure = .tooLarge }
        } catch is CancellationError {
            return
        } catch {
            if !Task.isCancelled { failure = .failed }
        }
    }

    private func open() {
        if let preview {
            presented = preview
        } else if failure != nil {
            openURL(file.url)
        }
    }
}

/// Shows the PDF's first page or offers to open the original when previewing fails.
struct PDFAttachmentCard: View {
    enum Phase {
        case loading
        case loaded(Image, size: Int)
        case failed
        case tooLarge
    }

    let file: SharedFile
    let phase: Phase
    let open: () -> Void
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    #endif

    var body: some View {
        Button(action: open) {
            MessageMediaCard {
                if isCompact {
                    HStack(spacing: Theme.Spacing.s) {
                        page.frame(width: 48, height: 60)
                        caption
                    }
                } else {
                    page.frame(height: 200)
                    caption
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(cardAction == nil)
        .accessibilityHint(Text(cardAction?.hint ?? ""))
    }

    private var caption: some View {
        HStack {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                AttachmentMediaCaption(file: file, kind: .pdf, downloadedSize: size)
                if let status {
                    Text(status)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
            }
            Spacer(minLength: 0)
            if let cardAction {
                Image(systemName: cardAction.symbol)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .accessibilityHidden(true)
            }
        }
    }

    private var page: some View {
        ZStack {
            Theme.Colors.surface2
            switch phase {
            case .loading:
                ProgressView().controlSize(.small)
            case let .loaded(image, _):
                image.resizable().scaledToFit()
            case .failed, .tooLarge:
                Image(systemName: "doc.richtext").foregroundStyle(Theme.Colors.fgMuted)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous))
        .accessibilityHidden(true)
    }

    private var size: Int? {
        if case let .loaded(_, size) = phase { return size }
        return nil
    }

    private var status: String? {
        switch phase {
        case .loading: "Loading PDF…"
        case .loaded: nil
        case .failed: "Couldn't load PDF"
        case .tooLarge: "PDF exceeds the preview size limit"
        }
    }

    private var cardAction: (symbol: String, hint: String)? {
        switch phase {
        case .loaded: ("eye", "Opens in Quick Look")
        case .failed, .tooLarge: ("arrow.up.right", "Opens the attachment")
        case .loading: nil
        }
    }

    private var isCompact: Bool {
        #if os(iOS)
        horizontalSizeClass == .compact
        #else
        false
        #endif
    }
}
