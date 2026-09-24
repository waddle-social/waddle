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
        let data = try await Self.download(url)
        cache.setObject(data as NSData, forKey: url as NSURL, cost: data.count)
        return data
    }

    /// Streams the body and stops at `sizeLimit`, so an oversized or
    /// endless response is never held in memory.
    private static func download(_ url: URL) async throws -> Data {
        let (bytes, response) = try await URLSession.shared.bytes(from: url)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else {
            bytes.task.cancel()
            throw RemoteImageLoadError.rejected(status: status)
        }
        guard response.expectedContentLength <= Int64(sizeLimit) else {
            bytes.task.cancel()
            throw RemoteImageLoadError.tooLarge
        }
        var data = Data()
        if response.expectedContentLength > 0 {
            data.reserveCapacity(Int(response.expectedContentLength))
        }
        for try await byte in bytes {
            guard data.count < sizeLimit else {
                bytes.task.cancel()
                throw RemoteImageLoadError.tooLarge
            }
            data.append(byte)
        }
        return data
    }
}
