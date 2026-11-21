import Foundation
import WaddleKit

/// XEP-0107 User Mood vocabulary. The schema defines a closed set of mood
/// elements, so only these values may be published.
enum ProfileMoodCatalog {
    /// Every mood element in the XEP-0107 schema.
    static let vocabulary: Set<String> = [
        "afraid", "amazed", "angry", "amorous", "annoyed", "anxious", "aroused",
        "ashamed", "bored", "brave", "calm", "cautious", "cold", "confident",
        "confused", "contemplative", "contented", "cranky", "crazy", "creative",
        "curious", "dejected", "depressed", "disappointed", "disgusted",
        "dismayed", "distracted", "embarrassed", "envious", "excited",
        "flirtatious", "frustrated", "grumpy", "guilty", "happy", "hopeful",
        "hot", "humbled", "humiliated", "hungry", "hurt", "impressed", "in_awe",
        "in_love", "indignant", "interested", "intoxicated", "invincible",
        "jealous", "lonely", "lucky", "mean", "moody", "nervous", "neutral",
        "offended", "outraged", "playful", "proud", "relaxed", "relieved",
        "remorseful", "restless", "sad", "sarcastic", "serious", "shocked",
        "shy", "sick", "sleepy", "spontaneous", "stressed", "strong",
        "surprised", "thankful", "thirsty", "tired", "undefined", "weak",
        "worried",
    ]

    /// The curated moods shown in the picker, in display order.
    static let curated: [ProfileChoice] = [
        choice("happy", "😊"),
        choice("excited", "🤩"),
        choice("relaxed", "😌"),
        choice("calm", "🧘"),
        choice("curious", "🧐"),
        choice("creative", "🎨"),
        choice("thankful", "🙏"),
        choice("playful", "😜"),
        choice("proud", "😎"),
        choice("hopeful", "🤞"),
        choice("contemplative", "🤔"),
        choice("surprised", "😮"),
        choice("tired", "🥱"),
        choice("sleepy", "😴"),
        choice("stressed", "😫"),
        choice("anxious", "😰"),
        choice("sad", "😢"),
        choice("sick", "🤒"),
        choice("hungry", "😋"),
        choice("grumpy", "😠"),
    ]

    private static func choice(_ value: String, _ emoji: String) -> ProfileChoice {
        ProfileChoice(value: value, emoji: emoji, title: title(for: value))
    }

    static func isValid(_ value: String) -> Bool {
        vocabulary.contains(value)
    }

    /// The curated option for `value`, if it is one.
    static func option(for value: String) -> ProfileChoice? {
        curated.first { $0.value == value }
    }

    /// The curated moods, preceded by `current` when it is a valid mood
    /// outside the curated set (for example one set from another client).
    static func options(including current: String?) -> [ProfileChoice] {
        guard let current, isValid(current), option(for: current) == nil else { return curated }
        return [choice(current, emoji(for: current))] + curated
    }

    /// Emoji for any mood; moods outside the curated set get a neutral glyph.
    static func emoji(for value: String) -> String {
        option(for: value)?.emoji ?? "💭"
    }

    /// Sentence-case label for a mood element name (`in_awe` → `In awe`).
    static func title(for value: String) -> String {
        let spaced = value.replacingOccurrences(of: "_", with: " ")
        guard let first = spaced.first else { return spaced }
        return first.uppercased() + spaced.dropFirst()
    }

    /// The mood to publish, or nil when `value` is not in the vocabulary.
    /// A blank note is dropped rather than published as empty text.
    static func mood(value: String, note: String) -> UserMood? {
        guard isValid(value) else { return nil }
        let trimmed = note.trimmingCharacters(in: .whitespacesAndNewlines)
        return UserMood(value: value, text: trimmed.isEmpty ? nil : trimmed)
    }
}
