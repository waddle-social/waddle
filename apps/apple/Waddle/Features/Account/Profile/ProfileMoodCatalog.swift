import Foundation
import WaddleKit

/// A mood offered in the profile picker.
struct ProfileMoodOption: Identifiable, Hashable, Sendable {
    /// XEP-0107 mood element name.
    let value: String
    /// Graphical representation, as XEP-0107 §i18n leaves presentation to
    /// the receiving application.
    let emoji: String

    var id: String { value }
    var title: String { ProfileMoodCatalog.title(for: value) }
}

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
    static let curated: [ProfileMoodOption] = [
        ProfileMoodOption(value: "happy", emoji: "😊"),
        ProfileMoodOption(value: "excited", emoji: "🤩"),
        ProfileMoodOption(value: "relaxed", emoji: "😌"),
        ProfileMoodOption(value: "calm", emoji: "🧘"),
        ProfileMoodOption(value: "curious", emoji: "🧐"),
        ProfileMoodOption(value: "creative", emoji: "🎨"),
        ProfileMoodOption(value: "thankful", emoji: "🙏"),
        ProfileMoodOption(value: "playful", emoji: "😜"),
        ProfileMoodOption(value: "proud", emoji: "😎"),
        ProfileMoodOption(value: "hopeful", emoji: "🤞"),
        ProfileMoodOption(value: "contemplative", emoji: "🤔"),
        ProfileMoodOption(value: "surprised", emoji: "😮"),
        ProfileMoodOption(value: "tired", emoji: "🥱"),
        ProfileMoodOption(value: "sleepy", emoji: "😴"),
        ProfileMoodOption(value: "stressed", emoji: "😫"),
        ProfileMoodOption(value: "anxious", emoji: "😰"),
        ProfileMoodOption(value: "sad", emoji: "😢"),
        ProfileMoodOption(value: "sick", emoji: "🤒"),
        ProfileMoodOption(value: "hungry", emoji: "😋"),
        ProfileMoodOption(value: "grumpy", emoji: "😠"),
    ]

    static func isValid(_ value: String) -> Bool {
        vocabulary.contains(value)
    }

    /// The curated option for `value`, if it is one.
    static func option(for value: String) -> ProfileMoodOption? {
        curated.first { $0.value == value }
    }

    /// The curated moods, preceded by `current` when it is a valid mood
    /// outside the curated set (for example one set from another client).
    static func options(including current: String?) -> [ProfileMoodOption] {
        guard let current, isValid(current), option(for: current) == nil else { return curated }
        return [ProfileMoodOption(value: current, emoji: emoji(for: current))] + curated
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
