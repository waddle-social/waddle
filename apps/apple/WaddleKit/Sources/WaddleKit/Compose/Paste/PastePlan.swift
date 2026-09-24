import Foundation

/// What to do with a paste: load one representation per item as an
/// attachment, or leave the paste to the text field.
public struct PastePlan: Hashable, Sendable {
    /// Per pasteboard item, the representation to load; nil loads nothing.
    public let loads: [PasteRepresentation?]

    public init(loads: [PasteRepresentation?], textItems: [Int] = []) {
        self.loads = loads
        self.textItems = textItems
    }

    /// No item offers a file or image: let the text field paste text.
    public var isTextPaste: Bool { loads.allSatisfy { $0 == nil } }

    /// Items to paste as text next to the attachments: those that offer
    /// plain text and nothing to attach, like a caption copied with an
    /// image. (A copied file's name rides on the file's own item, so it
    /// is never pasted.) Empty for a text paste, which the field handles.
    public let textItems: [Int]

    public static func plan(items: [PasteboardItem]) -> PastePlan {
        let loads = items.map(representation)
        guard loads.contains(where: { $0 != nil }) else { return PastePlan(loads: loads) }
        let textItems = items.indices.filter { index in
            loads[index] == nil && !Set(items[index].types).isDisjoint(with: plainTextTypes)
        }
        return PastePlan(loads: loads, textItems: textItems)
    }

    /// A file URL wins (it is the real file), then a GIF (keeps the
    /// animation), then the best still image. An item that offers plain
    /// text and HTML alongside a still image is a rich-text selection from
    /// an office or browser app, not a copied picture, so it pastes as text.
    /// (The web client inspects the HTML for text; this approximates that
    /// from the types alone.)
    static func representation(of item: PasteboardItem) -> PasteRepresentation? {
        let types = Set(item.types)
        if types.contains(PasteRepresentation.fileURLType) { return .fileURL }
        if types.contains(PasteRepresentation.gifType) { return .gif }
        guard let still = StillImageType.allCases.first(where: { types.contains($0.identifier) }) else { return nil }
        return isRichTextSelection(types) ? nil : .image(still)
    }

    private static let plainTextTypes: Set<String> = ["public.utf8-plain-text", "public.plain-text"]
    private static let htmlType = "public.html"

    private static func isRichTextSelection(_ types: Set<String>) -> Bool {
        types.contains(htmlType) && !types.isDisjoint(with: plainTextTypes)
    }
}
