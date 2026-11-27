import Testing
@testable import WaddleKit

struct ComposerSuggestionTriggerTests {
    @Test func mentionQueryEndsAtTheScalarCaret() {
        // The flag is one Character and two Unicode scalars.
        let text = "🇳🇴 @jo and @priya"
        let trigger = ComposerSuggestionTrigger.find(in: text, selection: 6..<6)
        #expect(trigger?.kind == .mention)
        #expect(trigger?.query == "jo")
        #expect(trigger?.replacementRange == 3..<6)
    }

    @Test func mentionUsesTheWebNonWhitespaceQuery() {
        #expect(ComposerSuggestionTrigger.find(in: "say @")?.query == "")
        #expect(ComposerSuggestionTrigger.find(in: "@a@b")?.query == "a@b")
        #expect(ComposerSuggestionTrigger.find(in: "mail@waddle.test") == nil)
        #expect(ComposerSuggestionTrigger.find(in: "(@jo") == nil)
        #expect(ComposerSuggestionTrigger.find(in: "hi\u{FEFF}@jo")?.query == "jo")
        #expect(ComposerSuggestionTrigger.find(in: "hi\u{0085}@jo") == nil)
        #expect(ComposerSuggestionTrigger.find(in: "@jo", selection: 1..<3) == nil)
        #expect(ComposerSuggestionTrigger.find(in: "@jo", selection: 4..<4) == nil)
    }

    @Test func emojiRequiresTwoASCIIShortcodeCharactersAfterABoundary() {
        let trigger = ComposerSuggestionTrigger.find(in: "say\n:Z_+-9 then", selection: 10..<10)
        #expect(trigger?.kind == .emoji)
        #expect(trigger?.query == "Z_+-9")
        for text in [":", ":a", ":a!", ":éa", "words:joy", "https://example.test"] {
            #expect(ComposerSuggestionTrigger.find(in: text) == nil)
        }
    }

    @Test func slashQueriesAtTheCaretAndReplacesTheLeadingWord() {
        let trigger = ComposerSuggestionTrigger.find(in: "/shrug hi", selection: 3..<3)
        #expect(trigger?.kind == .slash)
        #expect(trigger?.query == "sh")
        #expect(trigger?.replacementRange == 0..<6)
        #expect(ComposerSuggestionTrigger.find(in: "/shrug hi", selection: 6..<6)?.query == "shrug")
        #expect(ComposerSuggestionTrigger.find(in: "say /sh") == nil)
        #expect(ComposerSuggestionTrigger.find(in: " /sh") == nil)
        #expect(ComposerSuggestionTrigger.find(in: "/sh\nlater") == nil)
        #expect(ComposerSuggestionTrigger.find(in: "/ˢh") == nil)
        #expect(ComposerSuggestionTrigger.find(in: "/sho!", selection: 3..<3) == nil)
    }

    @Test func inlineTriggersTakePrecedenceOverSlashInTheArgument() {
        #expect(ComposerSuggestionTrigger.find(in: "/me @jo")?.kind == .mention)
        #expect(ComposerSuggestionTrigger.find(in: "/me :joy")?.kind == .emoji)
    }
}

struct SlashSuggestionTriggerTests {
    @Test func showsWhileTheCommandWordIsTyped() {
        #expect(ComposerSuggestionTrigger.find(in: "/")?.query == "")
        #expect(ComposerSuggestionTrigger.find(in: "/sh")?.query == "sh")
        #expect(ComposerSuggestionTrigger.find(in: "/ hello")?.query == "")
    }

    @Test func hidesOnceTheCommandIsChosenOrForPlainText() {
        #expect(ComposerSuggestionTrigger.find(in: "/shrug ") == nil)
        #expect(ComposerSuggestionTrigger.find(in: "/shrug hi") == nil)
        #expect(ComposerSuggestionTrigger.find(in: "hello") == nil)
        #expect(ComposerSuggestionTrigger.find(in: "/1") == nil)
    }
}
