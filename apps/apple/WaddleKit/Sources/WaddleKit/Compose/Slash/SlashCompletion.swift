/// Opening and completing a slash command while keeping its argument.
public enum SlashCompletion {
    public static func start(in text: String) -> ComposerTextEdit {
        if let command = SlashTrigger.parse(text) {
            let end = command.wordRange.upperBound
            return ComposerTextEdit(text: text, selection: end..<end)
        }
        return ComposerTextEdit(text: text.isEmpty ? "/" : "/ " + text, selection: 1..<1)
    }

    public static func complete(text: String, with candidate: SlashCandidate) -> String {
        let range = SlashTrigger.parse(text)?.wordRange ?? 0..<text.unicodeScalars.count
        return ComposerSuggestion.slash(candidate).accept(in: text, replacing: range).text
    }
}
