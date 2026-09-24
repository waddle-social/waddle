import Foundation

/// When the slash popover shows: while the command word is still being
/// typed (`/`, `/sh`), or for a bare `/` in front of text (`/ hello`, from
/// the composer's `/` button). Once a space follows a typed word the
/// command is chosen and the popover hides.
public enum SlashPopover {
    /// The prefix to filter candidates by, or nil when no popover shows.
    public static func prefix(in text: String) -> String? {
        guard let trigger = SlashTrigger.parse(text) else { return nil }
        if trigger.prefix.isEmpty { return "" }
        let typedWordOnly = text.unicodeScalars.count == 1 + trigger.prefix.unicodeScalars.count
        return typedWordOnly ? trigger.prefix : nil
    }
}
