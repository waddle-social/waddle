import Foundation

enum RemoteImageLoadError: Error, Equatable {
    case rejected(status: Int)
    case tooLarge
}

/// Fetches image bytes the timeline plays itself (GIFs), keeping recent
/// ones in memory so a row scrolled back into view does not refetch.
final class RemoteImageStore: @unchecked Sendable {
    static let shared = RemoteImageStore()

    /// The largest image a row fetches.
    static let sizeLimit = 20 * 1024 * 1024

    /// `NSCache` is thread-safe; entries cost their byte count.
    private let cache: NSCache<NSURL, NSData> = {
        let cache = NSCache<NSURL, NSData>()
        cache.totalCostLimit = 64 * 1024 * 1024
        return cache
    }()

    func cached(_ url: URL) -> Data? {
        cache.object(forKey: url as NSURL) as Data?
    }

    /// Cancelled with the calling task.
    func load(_ url: URL) async throws -> Data {
        if let data = cached(url) {
            return data
        }
        let (data, response) = try await URLSession.shared.data(from: url)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else { throw RemoteImageLoadError.rejected(status: status) }
        guard data.count <= Self.sizeLimit else { throw RemoteImageLoadError.tooLarge }
        cache.setObject(data as NSData, forKey: url as NSURL, cost: data.count)
        return data
    }
}
