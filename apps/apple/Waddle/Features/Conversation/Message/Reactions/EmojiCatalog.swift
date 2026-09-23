import Foundation

/// The reaction picker's emoji, grouped. XEP-0444 reactions are free-form
/// emoji, so this is only a convenient starting set.
enum EmojiCatalog {
    struct Group: Hashable, Identifiable {
        var id: String { title }
        let title: String
        let emoji: [String]
    }

    static let groups: [Group] = [
        Group(title: "Smileys", emoji: [
            "😀", "😃", "😄", "😁", "😆", "😅", "🤣", "😂", "🙂", "🙃", "😉", "😊",
            "😇", "🥰", "😍", "🤩", "😘", "😋", "😛", "😜", "🤪", "🤔", "🤨", "😐",
            "😑", "😶", "🙄", "😏", "😬", "😌", "😴", "🤯", "🥳", "😎", "🤓", "🧐",
            "😕", "😟", "😮", "😲", "😳", "🥺", "😢", "😭", "😱", "😤", "😡", "🤬",
        ]),
        Group(title: "Gestures", emoji: [
            "👍", "👎", "👌", "✌️", "🤞", "🤟", "🤘", "👋", "🙌", "👏", "🙏", "🤝",
            "💪", "👀", "🫡", "🤷", "🤦", "🙋",
        ]),
        Group(title: "Symbols", emoji: [
            "❤️", "🧡", "💛", "💚", "💙", "💜", "🖤", "🤍", "💔", "💯", "✅", "❌",
            "⚠️", "❓", "❗", "🔥", "✨", "⭐", "🎉", "🎊", "🚀", "💡", "📌", "🐧",
        ]),
        Group(title: "Things", emoji: [
            "☕", "🍕", "🍺", "🍰", "🎂", "🏆", "🎯", "🎮", "📚", "💻", "📷", "🎵",
            "🌈", "☀️", "🌙", "⚡", "🌊", "🌱",
        ]),
    ]
}
