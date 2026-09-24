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
    /// Whether a paste now would attach something rather than paste text.
    static var holdsAttachments: Bool {
        !plan().isTextPaste
    }

    static func read() -> ComposerPasteResult {
        let plan = plan()
        guard !plan.isTextPaste else { return .text(plainText()) }
        let text = plan.textItems.compactMap(plainText(itemAt:)).joined(separator: "\n")
        return .attachments(contents(for: plan), text: text.isEmpty ? nil : text)
    }

    private static func plan() -> PastePlan {
        PastePlan.plan(items: itemTypes().map { PasteboardItem(types: $0) })
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

    private static func plainText(itemAt index: Int) -> String? {
        guard let items = NSPasteboard.general.pasteboardItems, items.indices.contains(index) else { return nil }
        return items[index].string(forType: .string)
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

    private static func plainText(itemAt index: Int) -> String? {
        let items = UIPasteboard.general.items
        guard items.indices.contains(index) else { return nil }
        let item = items[index]
        return [UTType.utf8PlainText.identifier, UTType.plainText.identifier].lazy
            .compactMap { item[$0] as? String }
            .first
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
