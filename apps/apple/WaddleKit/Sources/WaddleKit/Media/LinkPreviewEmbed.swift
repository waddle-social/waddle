import Foundation

/// An iframe player at one of the final origins the server can seal.
public struct LinkPreviewEmbed: Hashable, Sendable {
    public enum Provider: String, Sendable {
        case youtube = "www.youtube-nocookie.com"
        case vimeo = "player.vimeo.com"
    }

    public static let defaultAspectRatio = 16.0 / 9.0

    public let url: URL
    public let aspectRatio: Double
    public let provider: Provider

    public init?(url: URL, width: UInt32? = nil, height: UInt32? = nil) {
        guard let provider = Self.provider(for: url) else { return nil }
        self.url = url
        self.provider = provider
        if let width, let height, width > 0, height > 0 {
            // Bound untrusted dimensions so a portrait player cannot fill the timeline.
            aspectRatio = min(3, max(0.5, Double(width) / Double(height)))
        } else {
            aspectRatio = Self.defaultAspectRatio
        }
    }

    private init(url: URL, provider: Provider, aspectRatio: Double) {
        self.url = url
        self.provider = provider
        self.aspectRatio = aspectRatio
    }

    static func provider(for url: URL) -> Provider? {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.scheme?.lowercased() == "https",
              components.user == nil, components.password == nil,
              components.rangeOfPort == nil || components.port == 443,
              let host = components.host?.lowercased()
        else { return nil }
        return Provider(rawValue: host)
    }

    func forPlayback(muted: Bool) -> Self? {
        guard var components = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return nil }
        let isYouTube = provider == .youtube
        let muteKey = isYouTube ? "mute" : "muted"
        let controlledKeys: Set<String> = ["autoplay", "mute", "muted", "playsinline"]
        // Preserve the provider's other parameters, including Vimeo's privacy hash.
        var items = (components.percentEncodedQueryItems ?? []).filter {
            !controlledKeys.contains($0.name.removingPercentEncoding ?? $0.name)
        }
        items.append(URLQueryItem(name: "autoplay", value: "1"))
        if isYouTube { items.append(URLQueryItem(name: "playsinline", value: "1")) }
        if muted { items.append(URLQueryItem(name: muteKey, value: "1")) }
        components.percentEncodedQueryItems = items
        // Only the query changed, so the validated provider origin is preserved.
        guard let playbackURL = components.url else { return nil }
        return Self(url: playbackURL, provider: provider, aspectRatio: aspectRatio)
    }
}
