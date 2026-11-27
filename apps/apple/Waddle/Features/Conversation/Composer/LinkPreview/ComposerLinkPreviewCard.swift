import SwiftUI
import WaddleKit

struct ComposerLinkPreviewCard: View {
    let state: ComposerLinkPreview.State
    let dismiss: () -> Void

    var body: some View {
        switch state {
        case .idle:
            EmptyView()
        case let .loading(url):
            status("Loading preview", url: url.rawValue, loading: true)
        case let .failed(url):
            status("Preview failed", url: url.rawValue)
        case let .unsupported(url):
            status("Preview unavailable", url: url.rawValue)
        case let .dismissed(url):
            status("Preview removed", url: url.rawValue, canDismiss: false)
        case let .ready(url, payload):
            ready(payload.preview, requestedURL: url.rawValue)
        }
    }

    private func ready(_ preview: LinkPreview, requestedURL: String) -> some View {
        let title = preview.title ?? host(of: requestedURL)
        let summary = preview.summary ?? preview.originalURL ?? requestedURL
        let display = LinkPreview(
            url: preview.url, title: title, summary: summary,
            imageURL: preview.imageURL, imageAlt: preview.imageAlt, originalURL: preview.originalURL
        )
        // The message card's own drawing, so a draft shows what will be sent. It is not a link
        // here: the one action on a draft's preview is removing it, from the card's corner.
        return ZStack(alignment: .topTrailing) {
            VStack {
                MessageLinkPreviewCard(preview: display, opensLink: false)
                    .accessibilityHidden(true)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text([host(of: preview.url.absoluteString), title, summary, preview.imageAlt]
                .compactMap { $0 }.joined(separator: ", ")))
            removeButton
                .padding(Theme.Spacing.s)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func status(_ title: LocalizedStringKey, url: String, loading: Bool = false, canDismiss: Bool = true) -> some View {
        HStack(spacing: Theme.Spacing.s) {
            Group {
                if loading {
                    ProgressView().controlSize(.small)
                } else {
                    Image(systemName: "doc.text")
                }
            }
            .foregroundStyle(Theme.Colors.fgMuted)
            .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(title)
                    .huddleText(Theme.Typography.caption.weight(700))
                    .foregroundStyle(Theme.Colors.fg)
                Text(host(of: url))
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
            if canDismiss { removeButton }
        }
        .padding(Theme.Spacing.s)
        .huddleSurface(.card, radius: Theme.Radius.medium)
    }

    private var removeButton: some View {
        Button(action: dismiss) {
            Image(systemName: "xmark.circle.fill")
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .buttonStyle(.plain)
        .help("Remove preview")
        .accessibilityLabel("Remove preview")
    }

    private func host(of url: String) -> String {
        let host = URL(string: url)?.host?.lowercased() ?? url
        return host.hasPrefix("www.") ? String(host.dropFirst(4)) : host
    }
}
