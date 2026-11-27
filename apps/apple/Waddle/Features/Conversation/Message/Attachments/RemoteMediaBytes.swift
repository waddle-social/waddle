import Foundation

/// Streams bounded HTTP responses for remote attachment previews.
enum RemoteMediaBytes {
    enum LoadError: Error, Equatable {
        case rejected(status: Int)
        case tooLarge
    }

    /// Streams the body and stops at the limit, so an oversized or
    /// endless response is never held in memory.
    static func download(_ url: URL, limit: Int) async throws -> Data {
        let (bytes, response) = try await URLSession.shared.bytes(from: url)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else {
            bytes.task.cancel()
            throw LoadError.rejected(status: status)
        }
        guard response.expectedContentLength <= Int64(limit) else {
            bytes.task.cancel()
            throw LoadError.tooLarge
        }
        var data = Data()
        if response.expectedContentLength > 0 {
            data.reserveCapacity(Int(response.expectedContentLength))
        }
        for try await byte in bytes {
            guard data.count < limit else {
                bytes.task.cancel()
                throw LoadError.tooLarge
            }
            data.append(byte)
        }
        return data
    }
}
