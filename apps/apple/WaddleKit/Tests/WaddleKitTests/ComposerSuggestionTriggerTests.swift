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

    @Test func roomTriggerUsesWebWhitespaceAndScalarOffsets() throws {
        let trigger = try #require(ComposerSuggestionTrigger.find(in: "🇳🇴\r\n#Café-٣_ tail", selection: 12..<12))
        #expect(trigger.kind == .room)
        #expect(trigger.query == "Café-٣_")
        #expect(trigger.replacementRange == 4..<12)
        #expect(ComposerSuggestionTrigger.find(in: "😀\u{FEFF}#ops")?.kind == .room)
        #expect(ComposerSuggestionTrigger.find(in: "#")?.kind == .room)
        #expect(ComposerSuggestionTrigger.find(in: "#ops", selection: 1..<4) == nil)
    }

    @Test(arguments: ["word#ops", "(#ops", "#a.b", "#a\u{301}", "#🙂"])
    func rejectsNonRoomTriggers(text: String) {
        #expect(ComposerSuggestionTrigger.find(in: text) == nil)
    }

    @Test func roomTriggersStayOutOfPairedCode() {
        #expect(ComposerSuggestionTrigger.find(in: "`inside #ops`", selection: 12..<12) == nil)
        #expect(ComposerSuggestionTrigger.find(in: "```swift\r\n#ops\r\n```", selection: 14..<14) == nil)
        #expect(ComposerSuggestionTrigger.find(in: "````\n#ops\n   ```", selection: 9..<9) == nil)
        #expect(ComposerSuggestionTrigger.find(in: "```\n#ops")?.kind == .room)
        #expect(ComposerSuggestionTrigger.find(in: "```\ncode\n```\n#ops")?.kind == .room)
        // A closed fence's opening line is dropped when the message is composed: no link can live on it.
        #expect(ComposerSuggestionTrigger.find(in: "```swift #ops\ncode\n```", selection: 13..<13) == nil)
        #expect(ComposerSuggestionTrigger.find(in: "`code` #ops")?.kind == .room)
        #expect(ComposerSuggestionTrigger.find(in: "**`inside #ops`**", selection: 14..<14)?.kind == .room)
    }

    @Test func mentionAndEmojiTriggersAreAvailableInCode() {
        #expect(ComposerSuggestionTrigger.find(in: "`inside @jo`", selection: 11..<11)?.kind == .mention)
        #expect(ComposerSuggestionTrigger.find(in: "```\n:joy\n```", selection: 8..<8)?.kind == .emoji)
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
