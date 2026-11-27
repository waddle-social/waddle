import Foundation

/// An HTTPS video source that follows the link preview media type policy.
public struct LinkPreviewVideo: Hashable, Sendable {
    public let url: URL
    public let isHLS: Bool

    public init?(url: URL, mediaType: String) {
        let type = Self.essence(mediaType)
        guard Self.isSupported(type), Self.isHTTPSWithoutCredentials(url) else { return nil }
        self.url = url
        isHLS = Self.hlsMediaTypes.contains(type)
    }

    init?(file: SharedFile) {
        guard file.disposition == .inline, file.encrypted == nil,
              let mediaType = file.mediaType
        else { return nil }
        self.init(url: file.url, mediaType: mediaType)
    }

    static func isHTTPSWithoutCredentials(_ url: URL) -> Bool {
        url.scheme?.lowercased() == "https" && url.host()?.isEmpty == false
            && url.user() == nil && url.password() == nil
    }

    static func isHLSMediaType(_ mediaType: String) -> Bool {
        hlsMediaTypes.contains(essence(mediaType))
    }

    private static func essence(_ mediaType: String) -> String {
        (mediaType.split(separator: ";", omittingEmptySubsequences: false).first ?? "")
            .trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    }

    private static func isSupported(_ mediaType: String) -> Bool {
        ["video/mp4", "video/webm", "video/ogg", "video/quicktime"].contains(mediaType)
            || hlsMediaTypes.contains(mediaType)
    }

    private static let hlsMediaTypes: Set<String> = [
        "application/vnd.apple.mpegurl", "application/x-mpegurl", "audio/x-mpegurl",
        "audio/mpegurl", "application/mpegurl",
    ]
}
