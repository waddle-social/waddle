import Foundation

/// Classifies an `/api/giphy` response. The proxy answers 503 when no key
/// is configured, 429 when rate limited and 502 on upstream failure, each
/// with an `{"error": …}` body.
public enum GifSearchResponse {
    public static let unavailableMessage = "GIF search is unavailable"

    public static func parse(data: Data, statusCode: Int) -> GifSearchResult {
        switch statusCode {
        case 200:
            guard let payload = try? JSONDecoder().decode(Payload.self, from: data) else {
                return .unavailable(message: unavailableMessage)
            }
            return .results((payload.data ?? []).compactMap(\.item))
        case 503:
            return .notConfigured
        default:
            let message = (try? JSONDecoder().decode(ErrorBody.self, from: data))?.error
            return .unavailable(message: message ?? unavailableMessage)
        }
    }

    private struct Payload: Decodable {
        let data: [Entry]?
    }

    private struct ErrorBody: Decodable {
        let error: String
    }

    /// A lenient entry: malformed fields decode to nil and the entry is
    /// skipped rather than failing the whole page.
    private struct Entry: Decodable {
        let id: String?
        let title: String?
        let previewURL: String?
        let originalURL: String?

        private enum Keys: String, CodingKey { case id, title, images }
        private enum ImageKeys: String, CodingKey {
            case fixedHeightSmall = "fixed_height_small"
            case original
        }
        private enum RenditionKeys: String, CodingKey { case url }

        init(from decoder: Decoder) throws {
            let container = try? decoder.container(keyedBy: Keys.self)
            let images = try? container?.nestedContainer(keyedBy: ImageKeys.self, forKey: .images)
            id = try? container?.decode(String.self, forKey: .id)
            title = try? container?.decode(String.self, forKey: .title)
            previewURL = Self.url(in: images, .fixedHeightSmall)
            originalURL = Self.url(in: images, .original)
        }

        private static func url(in images: KeyedDecodingContainer<ImageKeys>?, _ key: ImageKeys) -> String? {
            let rendition = try? images?.nestedContainer(keyedBy: RenditionKeys.self, forKey: key)
            return try? rendition?.decode(String.self, forKey: .url)
        }

        var item: GifSearchItem? {
            guard let id, let preview = httpsURL(previewURL), let original = httpsURL(originalURL) else { return nil }
            return GifSearchItem(id: id, title: title ?? "", previewURL: preview, originalURL: original)
        }

        private func httpsURL(_ raw: String?) -> URL? {
            guard let raw, let url = URL(string: raw), url.scheme?.lowercased() == "https", url.host != nil else {
                return nil
            }
            return url
        }
    }
}
