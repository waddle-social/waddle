import Foundation
import WaddleKit

/// One GIF search against the Waddle web app's Giphy proxy, which holds
/// the Giphy key. An empty query asks for trending GIFs.
struct GifSearchClient {
    var base: URL = GifSearchRequest.defaultBase
    var session: URLSession = .shared

    /// A transport failure reads as unavailable; a cancelled search returns
    /// the same, and callers drop it.
    func search(_ query: String) async -> GifSearchResult {
        let url = GifSearchRequest.url(base: base, query: query)
        do {
            let (data, response) = try await session.data(from: url)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            return GifSearchResponse.parse(data: data, statusCode: status)
        } catch {
            return .unavailable(message: GifSearchResponse.unavailableMessage)
        }
    }
}
