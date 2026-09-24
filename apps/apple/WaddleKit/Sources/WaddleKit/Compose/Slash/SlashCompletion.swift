import Foundation

/// Completing the typed `/prefix` to a picked candidate.
public enum SlashCompletion {
    /// Replaces the leading `/prefix` (and one following space, if any)
    /// with `/name `, keeping the rest: `/ hello` + shrug gives
    /// `/shrug hello`, `/sh` gives `/shrug `.
    public static func complete(text: String, with candidate: SlashCandidate) -> String {
        let completed = "/" + candidate.name + " "
        guard let trigger = SlashTrigger.parse(text) else { return completed }
        let rest = text.unicodeScalars.dropFirst(1 + trigger.prefix.unicodeScalars.count)
        let kept = rest.first == " " ? rest.dropFirst() : rest
        return completed + String(Substring(kept))
    }
}
