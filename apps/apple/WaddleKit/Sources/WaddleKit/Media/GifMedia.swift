import Foundation

/// Recognises GIFs, which the timeline plays instead of showing only their
/// first frame.
public enum GifMedia {
    /// A file declared `image/gif`, named `.gif`, or served from Giphy's
    /// media hosts.
    public static func isGIF(mediaType: String?, url: URL) -> Bool {
        if mediaType?.lowercased() == "image/gif" { return true }
        return url.pathExtension.lowercased() == "gif" || isGiphyMedia(url)
    }

    /// Bytes that start with the GIF87a or GIF89a signature.
    public static func isGIF(data: Data) -> Bool {
        signatures.contains { data.starts(with: $0) }
    }

    /// `https://media<N>.giphy.com/…` or `https://i.giphy.com/…` with a path.
    public static func isGiphyMedia(_ url: URL) -> Bool {
        guard url.scheme?.lowercased() == "https", let host = url.host?.lowercased(), url.path.count > 1 else { return false }
        if host == "i.giphy.com" { return true }
        guard host.hasPrefix("media"), host.hasSuffix(".giphy.com") else { return false }
        let digits = host.dropFirst("media".count).dropLast(".giphy.com".count)
        return digits.allSatisfy(\.isASCIIDigit)
    }

    private static let signatures: [[UInt8]] = [Array("GIF87a".utf8), Array("GIF89a".utf8)]
}

private extension Character {
    var isASCIIDigit: Bool { isASCII && isNumber }
}
