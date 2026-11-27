import Foundation

/// The session's media origin governs automatic preview image fetches.
public enum LinkPreviewMedia {
    /// Match the web's explicit auth origin and WebSocket fallback.
    public static func origin(explicit: String?, webSocket: String) -> URL? {
        let explicit = explicit?.trimmingCharacters(in: .whitespacesAndNewlines)
        let configured = explicit?.isEmpty == false ? explicit : nil
        guard var origin = URLComponents(string: configured ?? webSocket) else { return nil }
        switch (origin.scheme?.lowercased(), configured != nil) {
        case ("https", true), ("wss", false): origin.scheme = "https"
        case ("http", true), ("ws", false): origin.scheme = "http"
        default: return nil
        }
        guard origin.host?.isEmpty == false else { return nil }
        origin.user = nil
        origin.password = nil
        origin.path = ""
        origin.query = nil
        origin.fragment = nil
        return origin.url
    }

    /// Mirrors the origin and XEP-0363 path checks in chat/src/lib/xmpp/link-preview.ts.
    public static func trustedImageURL(_ raw: String?, mediaType: String?, origin: URL?) -> URL? {
        guard let raw, let image = URL(string: raw.trimmingCharacters(in: .whitespacesAndNewlines)),
              let origin, let host = image.host?.lowercased(), !host.isEmpty, host == origin.host?.lowercased(),
              let scheme = image.scheme?.lowercased(), scheme == origin.scheme?.lowercased(),
              scheme == "https" || (scheme == "http" && ["localhost", "127.0.0.1", "::1", "[::1]"].contains(host)),
              let mediaType = mediaType?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(),
              ["image/png", "image/jpeg", "image/gif", "image/webp"].contains(mediaType)
        else { return nil }
        let defaultPort = scheme == "https" ? 443 : 80
        guard (image.port ?? defaultPort) == (origin.port ?? defaultPort) else { return nil }
        let path = image.path(percentEncoded: true)
        let cachedPreview = /\/api\/files\/[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\/link-preview-[a-f0-9]{64}\.(png|jpg|gif|webp)/.ignoresCase()
        return path.wholeMatch(of: cachedPreview) != nil ? image : nil
    }
}
