import Testing
@testable import WaddleKit

struct ComposerSuggestionsTests {
    private let rows: [ComposerSuggestion] = [.slash(.builtin(.away)), .slash(.builtin(.active)), .slash(.builtin(.dnd))]

    @Test func arrowsWrapAndTheSelectedRowIsTheOneAccepted() throws {
        var selection = ComposerSuggestionSelection()
        let trigger = try #require(ComposerSuggestionTrigger.find(in: "/"))
        selection.update(trigger: trigger, items: rows)
        #expect(selection.selectedIndex == 0)
        selection.move(.up)
        #expect(selection.selected == rows[2])
        #expect(selection.selected?.accept(in: "/", replacing: trigger.replacementRange).text == "/dnd ")
        selection.move(.down)
        #expect(selection.selectedIndex == 0)
    }

    @Test func selectionFollowsTheItemAndClampsWhenItDisappears() throws {
        var selection = ComposerSuggestionSelection()
        let trigger = try #require(ComposerSuggestionTrigger.find(in: "/"))
        selection.update(trigger: trigger, items: rows)
        selection.move(.up)
        selection.update(trigger: trigger, items: rows)
        #expect(selection.selectedIndex == 2)
        selection.update(trigger: trigger, items: Array(rows.prefix(2)))
        #expect(selection.selectedIndex == 1)
        selection.update(trigger: trigger, items: Array(rows.suffix(2)))
        #expect(selection.selectedIndex == 0)
        selection.update(trigger: trigger, items: rows)
        #expect(selection.selectedIndex == 1)
        #expect(selection.selected == rows[1])
        selection.move(.down)
        let next = try #require(ComposerSuggestionTrigger.find(in: "/a"))
        selection.update(trigger: next, items: Array(rows.suffix(2)))
        #expect(selection.selectedIndex == 0)
        selection.update(trigger: nil, items: [])
        selection.move(.down)
        #expect(selection.selectedIndex == nil)
    }

    @Test func theFirstHighlightedItemAlsoSurvivesAnInsertionAheadOfIt() throws {
        var selection = ComposerSuggestionSelection()
        let trigger = try #require(ComposerSuggestionTrigger.find(in: "/"))
        selection.update(trigger: trigger, items: Array(rows.suffix(2)))
        selection.update(trigger: trigger, items: rows)
        #expect(selection.selected == rows[1])
    }

    @Test func dismissalSurvivesResultUpdatesUntilTheTriggerChanges() throws {
        var selection = ComposerSuggestionSelection()
        let trigger = try #require(ComposerSuggestionTrigger.find(in: "/"))
        selection.update(trigger: trigger, items: rows)
        selection.dismiss()
        selection.update(trigger: trigger, items: Array(rows.prefix(2)))
        #expect(selection.items.isEmpty)
        #expect(selection.selected == nil)
        let next = try #require(ComposerSuggestionTrigger.find(in: "/a"))
        selection.update(trigger: next, items: Array(rows.prefix(2)))
        #expect(selection.selected == rows[0])
    }

    @Test func emojiEditInsertsGlyphAndSpaceAndKeepsTheSuffix() throws {
        let text = "🇳🇴 :fami later"
        let trigger = try #require(ComposerSuggestionTrigger.find(in: text, selection: 8..<8))
        let emoji = ComposerSuggestion.emoji(.init(name: "family", glyph: "👨‍👩‍👧"))
        #expect(emoji.accept(in: text, replacing: trigger.replacementRange) ==
            ComposerTextEdit(text: "🇳🇴 👨‍👩‍👧  later", selection: 9..<9))
    }

    @Test func mentionEditKeepsTextAfterTheCaretAndRecordsThePickedTarget() throws {
        let text = "say @her suffix"
        let trigger = try #require(ComposerSuggestionTrigger.find(in: text, selection: 8..<8))
        let suggestion = ComposerSuggestion.mention(.init(name: "here", target: .here))
        let edit = suggestion.accept(in: text, replacing: trigger.replacementRange)
        #expect(edit == ComposerTextEdit(text: "say @here  suffix", selection: 10..<10))
        let mention = try #require(suggestion.recordedMention)
        #expect(MentionTokens.locate([mention], in: edit.text) == [MentionDraft(target: .here, range: 4..<9)])
    }

    @Test func slashEditReplacesTheWholeCommandAndConsumesOneSpace() throws {
        let text = "/shrink  hi"
        let trigger = try #require(ComposerSuggestionTrigger.find(in: text, selection: 3..<3))
        let suggestion = ComposerSuggestion.slash(.builtin(.shrug))
        #expect(suggestion.accept(in: text, replacing: trigger.replacementRange) ==
            ComposerTextEdit(text: "/shrug  hi", selection: 7..<7))
    }
}

struct EmojiShortcodesTests {
    @Test func searchesAliasesInWebOrderAndKeepsTheFirstGlyphForADuplicateKeyword() {
        #expect(EmojiShortcodes.search("Joy") == [
            .init(name: "joy", glyph: "😀"),
            .init(name: "face_with_tears_of_joy", glyph: "😂"),
            .init(name: "cat_with_tears_of_joy", glyph: "😹"),
            .init(name: "joystick", glyph: "🕹️"),
        ])
        #expect(EmojiShortcodes.search("+1") == [.init(name: "+1", glyph: "👍")])
        #expect(EmojiShortcodes.search("h").isEmpty)
        #expect(EmojiShortcodes.search("🚓").isEmpty)
        #expect(EmojiShortcodes.search("🚓\u{00A0}") == [.init(name: "🚓\u{00A0}cop", glyph: "🚔")])
        #expect(EmojiShortcodes.search(" heart").first == .init(name: "anatomical heart", glyph: "🫀"))
    }

    @Test func capsResultsAtEightAndKeepsJavaScriptIntegerKeyOrder() {
        let results = EmojiShortcodes.search("10")
        #expect(results.map(\.name) == ["10", "100", "101", "1000", "1030", "1100", "2100", "10:00"])
        #expect(results[1].glyph == "💯")
    }
}

struct MentionSuggestionTests {
    @Test func broadcastLabelsComeFirstAndNickMatchingUsesSubstringsWithoutAccents() {
        let people = [occupant("someone"), occupant("Chloé"), occupant("here")]
        let all = MentionCandidates.matching("", occupants: people, ownNick: "me")
        #expect(all.map(\.name) == ["everyone", "here", "Chloé", "someone"])
        #expect(MentionCandidates.matching("LOE", occupants: people, ownNick: "me").map(\.name) == ["Chloé"])
    }

    @Test func directoryFollowsOccupantsInServerOrderAndOnlyAcceptedRowsReserveNamesAndJIDs() throws {
        let ownJID = try #require(BareJID(parsing: "me@waddle.test"))
        let member = try #require(BareJID(parsing: "member@waddle.test"))
        let directoryFirst = try #require(BareJID(parsing: "first@waddle.test"))
        let directorySecond = try #require(BareJID(parsing: "second@waddle.test"))
        let directoryThird = try #require(BareJID(parsing: "third@waddle.test"))
        let directory: [UserSearchResult] = [
            .init(jid: ownJID, username: "MyOtherName", displayName: nil),
            .init(jid: member, username: "AnotherNick", displayName: nil),
            .init(jid: directoryFirst, username: "CHLOÉ", displayName: nil),
            .init(jid: directorySecond, username: "Zara", displayName: "Zara Display Name"),
            .init(jid: directorySecond, username: "ZaraAlias", displayName: nil),
            .init(jid: directoryThird, username: "zara", displayName: nil),
            .init(jid: directoryThird, username: "HERE", displayName: nil),
            .init(jid: directoryFirst, username: "Ana", displayName: nil),
            .init(jid: directoryThird, username: "Bea", displayName: nil),
        ]
        let people = [occupant("Chloé"), occupant("chloé"), occupant("me"), occupant("Anonymous", address: nil)]
        let result = MentionCandidates.matching("", occupants: people, ownNick: "me",
                                                directoryUsers: directory, ownJID: ownJID)
        #expect(result.map(\.name) == ["everyone", "here", "Chloé", "Zara", "Ana", "Bea"])
        #expect(result.compactMap(\.jid) == [member, directorySecond, directoryFirst, directoryThird])
    }

    private func occupant(_ nick: String, address: String? = "member@waddle.test") -> Occupant {
        Occupant(nick: nick, availability: .available, status: nil, affiliation: .member, role: .participant,
                 realJID: address.flatMap { BareJID(parsing: $0) }, hats: [])
    }
}
