import Foundation
import WaddleKit

/// Shares PDF downloads and keeps recent rendered previews available across rows.
actor PDFAttachmentStore {
    static let shared = PDFAttachmentStore()
    static let sizeLimit = 20 * 1024 * 1024

    private static let root = FileManager.default.temporaryDirectory
        .appendingPathComponent("PDFAttachmentPreviews", isDirectory: true)

    private let cache: NSCache<NSURL, PDFAttachmentPreview> = {
        let cache = NSCache<NSURL, PDFAttachmentPreview>()
        cache.totalCostLimit = 64 * 1024 * 1024
        return cache
    }()
    private var pending: [URL: Task<PDFAttachmentPreview, Error>] = [:]
    private var generation = 0

    private init() {
        // Clear leftovers before any scene can load a preview.
        try? FileManager.default.removeItem(at: Self.root)
    }

    func load(_ file: SharedFile) async throws -> PDFAttachmentPreview {
        try Task.checkCancellation()
        guard let url = file.inlineMediaURL else { throw URLError(.unsupportedURL) }
        guard (file.size ?? 0) <= Self.sizeLimit else { throw RemoteMediaBytes.LoadError.tooLarge }
        if let preview = cache.object(forKey: url as NSURL) { return preview }

        let generation = self.generation
        let task = pending[url] ?? startLoad(url, file: file)
        let preview = try await task.value
        guard generation == self.generation else { throw CancellationError() }
        return preview
    }

    func purge() {
        generation += 1
        for task in pending.values { task.cancel() }
        pending.removeAll()
        cache.removeAllObjects()
        try? FileManager.default.removeItem(at: Self.root)
    }

    private func startLoad(_ url: URL, file: SharedFile) -> Task<PDFAttachmentPreview, Error> {
        // Row cancellation leaves this shared task alive. Rendering and publishing
        // run on the store actor so purge cannot race a write into the preview root.
        let task = Task {
            defer {
                if !Task.isCancelled { pending[url] = nil }
            }
            let data = try await RemoteMediaBytes.download(url, limit: Self.sizeLimit)
            try Task.checkCancellation()
            let preview = try PDFAttachmentPreview.render(data, for: file, in: Self.root)
            let thumbnail = preview.thumbnail
            cache.setObject(preview, forKey: url as NSURL, cost: preview.size + thumbnail.bytesPerRow * thumbnail.height)
            return preview
        }
        pending[url] = task
        return task
    }
}
