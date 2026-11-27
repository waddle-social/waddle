import Foundation

/// Server-attached XEP-0511 card.
public struct LinkPreview: Hashable, Sendable {
    public let url: URL
    /// The URL identified in the message, before the resolver canonicalized it.
    public let originalURL: String?
    public let title: String?
    public let summary: String?
    public let imageURL: URL?
    public let imageAlt: String?
    public let playback: LinkPreviewPlayback?

    public init(
        url: URL, title: String?, summary: String?, imageURL: URL?, imageAlt: String?, originalURL: String?,
        playback: LinkPreviewPlayback? = nil
    ) {
        self.url = url
        self.originalURL = originalURL
        self.title = title
        self.summary = summary
        self.imageURL = imageURL
        self.imageAlt = imageAlt
        self.playback = playback
    }

    func withPlayback(_ playback: LinkPreviewPlayback) -> Self {
        Self(
            url: url, title: title, summary: summary, imageURL: imageURL, imageAlt: imageAlt,
            originalURL: originalURL, playback: playback
        )
    }

    func withoutMedia() -> Self {
        Self(url: url, title: title, summary: summary, imageURL: nil, imageAlt: nil, originalURL: originalURL)
    }
}
