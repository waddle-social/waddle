import PDFKit
import WaddleKit

/// Quick Look reads the same temporary copy as the thumbnail renderer.
final class PDFAttachmentPreview: Sendable {
    let url: URL
    let thumbnail: CGImage
    let size: Int

    private init(url: URL, thumbnail: CGImage, size: Int) {
        self.url = url
        self.thumbnail = thumbnail
        self.size = size
    }

    deinit {
        try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
    }

    static func render(_ data: Data, for file: SharedFile, in root: URL) throws -> PDFAttachmentPreview {
        let files = FileManager.default
        let folder = root.appendingPathComponent(UUID().uuidString, isDirectory: true)
        let name = file.localFileName
        // Quick Look selects a renderer using the extension.
        let url = folder.appendingPathComponent(
            (name as NSString).pathExtension.lowercased() == "pdf" ? name : "\(name).pdf"
        )
        try files.createDirectory(at: folder, withIntermediateDirectories: true)
        do {
            try data.write(to: url)
            guard let document = PDFDocument(url: url), !document.isLocked, let page = document.page(at: 0) else {
                throw URLError(.cannotDecodeContentData)
            }
            let thumbnail = withExtendedLifetime(document) {
                page.thumbnail(of: CGSize(width: 320, height: 440), for: .cropBox)
            }
            #if os(macOS)
            let cgImage = thumbnail.cgImage(forProposedRect: nil, context: nil, hints: nil)
            #else
            let cgImage = thumbnail.cgImage
            #endif
            guard let cgImage else { throw URLError(.cannotDecodeContentData) }
            return Self(url: url, thumbnail: cgImage, size: data.count)
        } catch {
            try? files.removeItem(at: folder)
            throw error
        }
    }
}
