/// The web edit form sends a correction for changed content or a changed
/// card. An unchanged edit with the same card needs no correction.
public struct ComposerLinkPreviewEdit {
    private let contentChanged: Bool
    private let originalPreview: LinkPreview?
    private let removesPreview: Bool
    public let shouldResolve: Bool

    public init(item: TimelineItem, draft: Draft, state: ComposerLinkPreview.State) {
        let original = EditableMessage(item: item)
        self.init(
            original: Draft(text: original.text, references: original.references),
            originalPreview: item.message.linkPreviews.first, draft: draft, state: state
        )
    }

    public init(original: Draft, originalPreview: LinkPreview?, draft: Draft, state: ComposerLinkPreview.State) {
        let baseline = MessageComposer.compose(original)
        let edited = MessageComposer.compose(draft)
        contentChanged = baseline?.body != edited?.body
            || baseline?.options.markupSpans != edited?.options.markupSpans
            || baseline?.options.references != edited?.options.references
        self.originalPreview = originalPreview
        let originalURL = originalPreview?.originalURL.map(ComposerLinkPreview.normalizedURL)
        let removedURL = originalURL.map { url in
            Self.contains(url, in: baseline?.body) && !Self.contains(url, in: edited?.body)
        } ?? false
        removesPreview = originalPreview != nil && (removedURL || state.isDismissed)
        shouldResolve = contentChanged || removesPreview || state.isReady
    }

    public func shouldSend(_ payload: LinkPreviewPayload?) -> Bool {
        if contentChanged { return true }
        guard let originalPreview else { return payload != nil }
        guard !removesPreview else { return true }
        guard let replacement = payload?.preview else { return false }
        let originalURL = originalPreview.originalURL ?? originalPreview.url.absoluteString
        let replacementURL = replacement.originalURL ?? replacement.url.absoluteString
        return ComposerLinkPreview.normalizedURL(originalURL) != ComposerLinkPreview.normalizedURL(replacementURL)
    }

    private static func contains(_ normalizedURL: String, in body: String?) -> Bool {
        guard let body else { return false }
        return ComposerLinkPreview.eligibleURLs(in: body).contains {
            ComposerLinkPreview.normalizedURL($0.rawValue) == normalizedURL
        }
    }
}
