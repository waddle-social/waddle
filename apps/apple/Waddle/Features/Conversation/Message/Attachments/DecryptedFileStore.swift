import Foundation
import UniformTypeIdentifiers
import WaddleKit

/// Decrypted attachments on disk, only while Quick Look shows one: Quick
/// Look opens files, not bytes. Each file is removed when its preview
/// closes, and a new launch removes whatever an earlier one left behind.
enum DecryptedFileStore {
    private static let directory: URL = {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("DecryptedAttachments", isDirectory: true)
        try? FileManager.default.removeItem(at: root)
        return root.appendingPathComponent(UUID().uuidString, isDirectory: true)
    }()

    #if os(iOS)
    private static let writingOptions: Data.WritingOptions = [.atomic, .completeFileProtection]
    #else
    private static let writingOptions: Data.WritingOptions = [.atomic]
    #endif

    /// Writes `data` under the file's own name, in a folder of its own so
    /// equal names never collide.
    static func write(_ data: Data, for file: SharedFile) throws -> URL {
        let folder = directory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let url = folder.appendingPathComponent(fileName(for: file))
        try data.write(to: url, options: writingOptions)
        return url
    }

    static func remove(_ url: URL) {
        try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
    }

    /// Quick Look picks a viewer by extension, so a name without one gets
    /// the media type's.
    private static func fileName(for file: SharedFile) -> String {
        let name = file.localFileName
        guard (name as NSString).pathExtension.isEmpty,
              let mediaType = file.mediaType,
              let fileExtension = UTType(mimeType: mediaType)?.preferredFilenameExtension
        else { return name }
        return "\(name).\(fileExtension)"
    }
}
