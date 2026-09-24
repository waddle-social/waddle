import Foundation

/// One pasteboard item, as the UTType identifiers it offers
/// (`public.png`, `com.compuserve.gif`, `public.file-url`, …).
public struct PasteboardItem: Hashable, Sendable {
    public let types: [String]

    public init(types: [String]) {
        self.types = types
    }
}
