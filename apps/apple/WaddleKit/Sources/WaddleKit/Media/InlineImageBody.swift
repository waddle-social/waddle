import Foundation

/// A body that is nothing but an image link renders as that image, the way
/// the web client shows a shared GIF: an `https` URL whose path ends in an
/// image extension, or a Giphy media URL.
public enum InlineImageBody {
    static let imageExtensions: Set<String> = ["gif", "png", "jpg", "jpeg", "webp"]

    /// The image URL when the trimmed body is a single `https` image URL.
    public static func url(in body: String) -> URL? {
        let trimmed = body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              !trimmed.unicodeScalars.contains(where: { CharacterSet.whitespacesAndNewlines.contains($0) }),
              let url = URL(string: trimmed),
              url.scheme?.lowercased() == "https",
              url.host?.isEmpty == false
        else { return nil }
        if imageExtensions.contains(url.pathExtension.lowercased()) || GifMedia.isGiphyMedia(url) {
            return url
        }
        return nil
    }
}
