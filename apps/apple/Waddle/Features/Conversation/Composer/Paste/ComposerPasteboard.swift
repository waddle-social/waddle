import Foundation
import UniformTypeIdentifiers
import WaddleKit
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// Reads the system pasteboard through WaddleKit's `PastePlan`: a copied
/// file, GIF or picture becomes an attachment; anything else pastes as
/// text.
@MainActor
enum ComposerPasteboard {
    /// Types the Mac field's paste command claims.
    static let attachmentTypes: [UTType] = [.fileURL, .gif, .png, .jpeg, .tiff, .heic, .image]

    static func read() -> ComposerPasteResult {
        let items = itemTypes().map { PasteboardItem(types: $0) }
        let plan = PastePlan.plan(items: items)
        guard !plan.isTextPaste else { return .text(plainText()) }
        return .attachments(contents(for: plan))
    }

    private static func contents(for plan: PastePlan) -> [ComposerPasteContent] {
        var fileURLs = copiedFileURLs()
        var contents: [ComposerPasteContent] = []
        for (index, load) in plan.loads.enumerated() {
            guard let load else { continue }
            switch load {
            case .fileURL:
                guard !fileURLs.isEmpty else { continue }
                contents.append(.file(fileURLs.removeFirst()))
            case .gif:
                if let bytes = data(ofType: UTType.gif.identifier, itemAt: index) {
                    contents.append(.image(bytes, mediaType: "image/gif", fileExtension: "gif"))
                }
            case let .image(still):
                if let bytes = data(ofType: still.identifier, itemAt: index) {
                    let type = UTType(still.identifier)
                    contents.append(.image(bytes, mediaType: type?.preferredMIMEType, fileExtension: type?.preferredFilenameExtension))
                }
            }
        }
        return contents
    }

    #if os(macOS)
    private static func itemTypes() -> [[String]] {
        (NSPasteboard.general.pasteboardItems ?? []).map { item in item.types.map(\.rawValue) }
    }

    private static func plainText() -> String? {
        NSPasteboard.general.string(forType: .string)
    }

    /// Read as URL objects so the sandbox grants access to the files.
    private static func copiedFileURLs() -> [URL] {
        let objects = NSPasteboard.general.readObjects(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        )
        return (objects as? [URL]) ?? []
    }

    private static func data(ofType identifier: String, itemAt index: Int) -> Data? {
        guard let items = NSPasteboard.general.pasteboardItems, items.indices.contains(index) else { return nil }
        return items[index].data(forType: NSPasteboard.PasteboardType(identifier))
    }
    #else
    private static func itemTypes() -> [[String]] {
        UIPasteboard.general.items.map { Array($0.keys) }
    }

    private static func plainText() -> String? {
        UIPasteboard.general.string
    }

    private static func copiedFileURLs() -> [URL] {
        UIPasteboard.general.items.compactMap { item in
            item[UTType.fileURL.identifier].flatMap(fileURL(from:))
        }
    }

    private static func fileURL(from value: Any) -> URL? {
        if let url = value as? URL { return url.isFileURL ? url : nil }
        if let string = value as? String, let url = URL(string: string) { return url.isFileURL ? url : nil }
        if let bytes = value as? Data {
            let url = URL(dataRepresentation: bytes, relativeTo: nil)
            return (url?.isFileURL ?? false) ? url : nil
        }
        return nil
    }

    /// Image values arrive as bytes or, for some sources, as a `UIImage`,
    /// which is re-encoded as PNG.
    private static func data(ofType identifier: String, itemAt index: Int) -> Data? {
        let items = UIPasteboard.general.items
        guard items.indices.contains(index), let value = items[index][identifier] else { return nil }
        if let bytes = value as? Data { return bytes }
        if let image = value as? UIImage { return image.pngData() }
        return nil
    }
    #endif
}
