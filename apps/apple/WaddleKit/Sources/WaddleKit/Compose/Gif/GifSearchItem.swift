import Foundation

/// One GIF search hit.
public struct GifSearchItem: Hashable, Sendable, Identifiable {
    public let id: String
    public let title: String
    /// Small, fixed-height rendition for the picker grid.
    public let previewURL: URL
    /// The full GIF that gets shared.
    public let originalURL: URL

    public init(id: String, title: String, previewURL: URL, originalURL: URL) {
        self.id = id
        self.title = title
        self.previewURL = previewURL
        self.originalURL = originalURL
    }
}

/// What a GIF search produced.
public enum GifSearchResult: Hashable, Sendable {
    case results([GifSearchItem])
    /// The proxy has no Giphy key configured.
    case notConfigured
    /// Rate limited, upstream failure or an unreadable response.
    case unavailable(message: String)
}
