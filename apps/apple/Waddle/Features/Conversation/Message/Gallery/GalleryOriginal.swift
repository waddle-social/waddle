import CoreTransferable
import Foundation
import UniformTypeIdentifiers
import WaddleKit

/// The original bytes stay on disk while the system save panel owns the export.
struct GalleryOriginal: Transferable {
    private let folder: URL
    private let url: URL
    var name: String { url.lastPathComponent }

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(exportedContentType: .data) { SentTransferredFile($0.url) }
    }

    static func download(_ file: SharedFile) async throws -> Self {
        let (download, response) = try await URLSession.shared.download(from: file.url)
        let files = FileManager.default
        defer { try? files.removeItem(at: download) }
        guard let response = response as? HTTPURLResponse, (200..<300).contains(response.statusCode) else {
            throw URLError(.badServerResponse)
        }
        try Task.checkCancellation()
        let folder = files.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        let url = folder.appendingPathComponent(file.localFileName)
        try files.createDirectory(at: folder, withIntermediateDirectories: false)
        do {
            try files.moveItem(at: download, to: url)
        } catch {
            try? files.removeItem(at: folder)
            throw error
        }
        return Self(folder: folder, url: url)
    }

    func remove() {
        try? FileManager.default.removeItem(at: folder)
    }
}
