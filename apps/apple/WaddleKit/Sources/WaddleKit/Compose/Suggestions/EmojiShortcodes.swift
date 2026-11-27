import Foundation

public struct EmojiShortcode: Decodable, Hashable, Sendable {
    public let name: String
    public let glyph: String

    public init(name: String, glyph: String) {
        self.name = name
        self.glyph = glyph
    }
}

/// Looks up aliases in the order generated from chat's emojilib index.
public enum EmojiShortcodes {
    static let minimumQueryLength = 2

    public static func search(_ query: String) -> [EmojiShortcode] {
        guard query.unicodeScalars.count >= minimumQueryLength else { return [] }
        let needle = query.lowercased()
        return Array(index.lazy.filter { $0.name.contains(needle) }.prefix(ComposerSuggestion.maximumRows))
    }

    private static let index: [EmojiShortcode] = {
        guard let url = Bundle.module.url(forResource: "EmojiShortcodes", withExtension: "json") else {
            assertionFailure("EmojiShortcodes.json is missing from WaddleKit's resources.")
            return []
        }
        do {
            return try JSONDecoder().decode([EmojiShortcode].self, from: Data(contentsOf: url))
        } catch {
            assertionFailure("Could not load EmojiShortcodes.json: \(error)")
            return []
        }
    }()
}
