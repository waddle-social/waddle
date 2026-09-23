import Foundation

enum EncryptedFileDownloadError: Error, Equatable {
    /// XEP-0448 sources without an `https` URL are not fetched.
    case noHTTPSSource
    case rejected(status: Int)
    case tooLarge
}

/// HTTP GET of XEP-0448 ciphertext from its XEP-0447 URL source, refused
/// past `limit` bytes.
enum EncryptedFileDownloader {
    static func download(from url: URL, limit: Int) async throws -> Data {
        let (bytes, response) = try await URLSession.shared.bytes(from: url)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else { throw EncryptedFileDownloadError.rejected(status: status) }
        guard response.expectedContentLength <= Int64(limit) else { throw EncryptedFileDownloadError.tooLarge }
        var data = Data()
        if response.expectedContentLength > 0 {
            data.reserveCapacity(Int(response.expectedContentLength))
        }
        for try await byte in bytes {
            guard data.count < limit else { throw EncryptedFileDownloadError.tooLarge }
            data.append(byte)
        }
        return data
    }
}
