import Foundation
import WaddleKit

/// XEP-0108 User Activity vocabulary: the general activities offered in
/// the picker and the specific ones the XEP defines under each.
enum ProfileActivityCatalog {
    private struct Entry {
        let value: String
        let emoji: String
        let title: String
        let specifics: [String]
    }

    /// `talking` is left out of the picker: `talking` with `on_the_phone`
    /// is how a call in progress is shown to contacts (ADR-010), so picking
    /// it by hand would read as being in a call. `undefined` says nothing.
    private static let offered: [Entry] = [
        Entry(value: "working", emoji: "💼", title: "Working", specifics: ["coding", "in_a_meeting", "studying", "writing"]),
        Entry(value: "relaxing", emoji: "🛋️", title: "Relaxing", specifics: [
            "fishing", "gaming", "going_out", "partying", "reading", "rehearsing", "shopping", "smoking",
            "socializing", "sunbathing", "watching_a_movie", "watching_tv",
        ]),
        Entry(value: "eating", emoji: "🍽️", title: "Eating", specifics: [
            "having_a_snack", "having_breakfast", "having_dinner", "having_lunch",
        ]),
        Entry(value: "drinking", emoji: "☕", title: "Drinking", specifics: ["having_a_beer", "having_coffee", "having_tea"]),
        Entry(value: "exercising", emoji: "🏃", title: "Exercising", specifics: [
            "cycling", "dancing", "hiking", "jogging", "playing_sports", "running", "skiing", "swimming", "working_out",
        ]),
        Entry(value: "traveling", emoji: "✈️", title: "Travelling", specifics: [
            "commuting", "cycling", "driving", "in_a_car", "on_a_bus", "on_a_plane", "on_a_train", "on_a_trip", "walking",
        ]),
        Entry(value: "doing_chores", emoji: "🧹", title: "Chores", specifics: [
            "buying_groceries", "cleaning", "cooking", "doing_maintenance", "doing_the_dishes", "doing_the_laundry",
            "gardening", "running_an_errand", "walking_the_dog",
        ]),
        Entry(value: "grooming", emoji: "🚿", title: "Grooming", specifics: [
            "at_the_spa", "brushing_teeth", "getting_a_haircut", "shaving", "taking_a_bath", "taking_a_shower",
        ]),
        Entry(value: "having_appointment", emoji: "📅", title: "Appointment", specifics: []),
        Entry(value: "inactive", emoji: "🌙", title: "Time off", specifics: [
            "day_off", "hanging_out", "hiding", "on_vacation", "praying", "scheduled_holiday", "sleeping", "thinking",
        ]),
    ]

    /// General activities the picker does not offer but another client may
    /// have published. `on_the_phone` and `on_video_phone` are never
    /// offered under `talking`: they mark a call (`UserActivity.showsACall`).
    private static let others: [Entry] = [
        Entry(value: "talking", emoji: "💬", title: "Talking", specifics: ["in_real_life"]),
        Entry(value: "undefined", emoji: "💭", title: "Something else", specifics: []),
    ]

    private static let specificTitles = ["watching_tv": "Watching TV"]

    private static func entry(for general: String) -> Entry? {
        (offered + others).first { $0.value == general }
    }

    private static func choice(_ entry: Entry) -> ProfileChoice {
        ProfileChoice(value: entry.value, emoji: entry.emoji, title: entry.title)
    }

    /// The offered activities, preceded by `current` when it is a known
    /// one outside them (for example a call shown by another device).
    static func choices(including current: String?) -> [ProfileChoice] {
        let extra = others.filter { $0.value == current }
        return (extra + offered).map(choice)
    }

    /// The specific activities defined under `general`, and `current`'s
    /// own when it is one the list lacks (another app may publish others).
    static func specifics(of general: String?, including current: UserActivity?) -> [String] {
        let defined = general.flatMap(entry)?.specifics ?? []
        guard let current, !current.showsACall, let kept = current.specific,
              current.general == general, !defined.contains(kept)
        else { return defined }
        return [kept] + defined
    }

    static func emoji(for general: String) -> String {
        entry(for: general)?.emoji ?? "💭"
    }

    static func title(for general: String) -> String {
        entry(for: general)?.title ?? ProfileMoodCatalog.title(for: general)
    }

    static func specificTitle(for specific: String) -> String {
        specificTitles[specific] ?? ProfileMoodCatalog.title(for: specific)
    }

    /// "Working · Coding", or the general activity alone.
    static func summary(of activity: UserActivity) -> String {
        guard let specific = activity.specific else { return title(for: activity.general) }
        return "\(title(for: activity.general)) · \(specificTitle(for: specific))"
    }

    /// The activity to publish, or nil when `general` is not one the XEP
    /// defines. A blank note is left out.
    static func activity(general: String, specific: String?, note: String) -> UserActivity? {
        guard entry(for: general) != nil else { return nil }
        let trimmed = note.trimmingCharacters(in: .whitespacesAndNewlines)
        return UserActivity(general: general, specific: specific, text: trimmed.isEmpty ? nil : trimmed)
    }
}
