import Foundation

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
        let data = try await RemoteMediaBytes.download(url, limit: Self.sizeLimit)
        cache.setObject(data as NSData, forKey: url as NSURL, cost: data.count)
        return data
    }
}
