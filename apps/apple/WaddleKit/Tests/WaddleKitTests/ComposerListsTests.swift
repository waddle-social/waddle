import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

struct ComposerListsTests {
    @Test func listShortcutsUsePhysicalKeysOnBothPlatforms() {
        #expect(ComposerFormat.listShortcut(for: .ansi(0x1A)) == .orderedList)
        #expect(ComposerFormat.listShortcut(for: .ansi(0x1C)) == .bulletList)
        #expect(ComposerFormat.listShortcut(for: .usb(0x24)) == .orderedList)
        #expect(ComposerFormat.listShortcut(for: .usb(0x25)) == .bulletList)
        #expect(ComposerFormat.listShortcut(for: .usb(0x26)) == nil)
    }

    @Test(arguments: [ComposerFormat.bulletList, .orderedList])
    func togglesTheWholeLineAroundAPartialSelection(_ format: ComposerFormat) {
        let marker = format == .bulletList ? "- " : "1. "
        let edit = ComposerFormatting.apply(format, to: "hi 👍 there", selection: 3..<4)
        #expect(edit.text == marker + "hi 👍 there")
        #expect(edit.selection == (3 + marker.count)..<(4 + marker.count))
        let undone = ComposerFormatting.apply(format, to: edit.text, selection: edit.selection)
        #expect(undone.text == "hi 👍 there")
        #expect(undone.selection == 3..<4)
    }

    @Test(arguments: [ComposerFormat.bulletList, .orderedList])
    func togglesSeveralTouchedLinesAndExcludesTheNextLine(_ format: ComposerFormat) {
        let edit = ComposerFormatting.apply(format, to: "one\ntwo\nthree", selection: 1..<8)
        #expect(edit.text == (format == .bulletList ? "- one\n- two\nthree" : "1. one\n2. two\nthree"))
        let undone = ComposerFormatting.apply(format, to: edit.text, selection: edit.selection)
        #expect(undone.text == "one\ntwo\nthree")
        #expect(undone.selection == 1..<8)
    }

    @Test func switchingKindConvertsTheEnclosingList() {
        let ordered = ComposerFormatting.apply(.orderedList, to: "- one\n- two\n- three", selection: 8..<9)
        #expect(ordered.text == "1. one\n2. two\n3. three")
        let bullets = ComposerFormatting.apply(.bulletList, to: ordered.text, selection: ordered.selection)
        #expect(bullets.text == "- one\n- two\n- three")
        #expect(bullets.selection == 8..<9)
    }

    @Test func liftingAMiddleItemRestartsTheFollowingOrderedList() {
        let edit = ComposerFormatting.apply(.orderedList, to: "1. a\n2. b\n3. c", selection: 9..<9)
        #expect(edit.text == "1. a\nb\n1. c")
        #expect(edit.selection == 6..<6)
    }

    @Test func togglingOrderedItemsLeavesLinesOutsideTheirRunAlone() {
        let text = "1. a\n2. b\n2024. was a good year\n3. c"
        #expect(ComposerFormatting.apply(.orderedList, to: text, selection: 4..<4).text
            == "a\n1. b\n2024. was a good year\n3. c")
        #expect(ComposerFormatting.apply(.bulletList, to: text, selection: 4..<4).text
            == "- a\n- b\n2024. was a good year\n3. c")
        #expect(ComposerFormatting.apply(.orderedList, to: "a\n2024. was a good year", selection: 0..<0).text
            == "1. a\n2024. was a good year")
    }

    @Test func addsAnEmptyItemAtTheCaret() {
        let edit = ComposerFormatting.apply(.bulletList, to: "", selection: nil)
        #expect(edit.text == "- ")
        #expect(edit.selection == 2..<2)
    }

    @Test func wrappingPlainTextLeavesUnselectedIndentationAlone() {
        let edit = ComposerFormatting.apply(.bulletList, to: "one\n  two", selection: 0..<0)
        #expect(edit.text == "- one\n  two")
    }

    @Test func wrappingKeepsPlainLeadingSpacesAsItemText() {
        let edit = ComposerFormatting.apply(.orderedList, to: "first\n  padded ", selection: 0..<15)
        #expect(edit.text == "1. first\n2.   padded ")
    }

    @Test func togglingAnIndentedItemOnlyRemovesItsMarker() {
        let edit = ComposerFormatting.apply(.bulletList, to: "9. a\n   - b\n   - c\n10. d", selection: 11..<11)
        #expect(edit.text == "9. a\n   b\n   - c\n10. d")
        #expect(edit.selection == 9..<9)
    }

    @Test(arguments: ["*", "+"])
    func togglesOffOtherBulletMarkers(_ marker: String) {
        let edit = ComposerFormatting.apply(.bulletList, to: "\(marker) item", selection: nil)
        #expect(edit.text == "item")
        #expect(edit.selection == 0..<4)
    }

    @Test func listsCanBeInsideQuotes() {
        let edit = ComposerFormatting.apply(.orderedList, to: "> one\n> two", selection: 2..<11)
        #expect(edit.text == "> 1. one\n> 2. two")
        #expect(edit.selection == 5..<17)
    }

    @Test func convertsAnEntireCodeBlockToOneListItem() {
        let text = "```swift\none\ntwo\n```"
        let edit = ComposerFormatting.apply(.orderedList, to: text, selection: 10..<10)
        #expect(edit.text == "1. one\ntwo")
        #expect(edit.selection == 4..<4)
    }

    @Test func numberingAndSelectionAccountForCRLFAndTwoDigitMarkers() {
        let text = Array(repeating: "👍", count: 11).joined(separator: "\r\n")
        let edit = ComposerFormatting.apply(.orderedList, to: text, selection: nil)
        #expect(edit.text == (1...11).map { "\($0). 👍" }.joined(separator: "\r\n"))
        #expect(edit.selection == 3..<edit.text.unicodeScalars.count)
    }

    @Test(arguments: [ComposerFormat.bulletList, .orderedList])
    func markersArePartOfTheBodyAndCarryNoMarkup(_ format: ComposerFormat) throws {
        let edit = ComposerFormatting.apply(format, to: "one\ntwo", selection: nil)
        let sent = try #require(MessageComposer.compose(.init(text: edit.text)))
        #expect(sent.body == (format == .bulletList ? "- one\n- two" : "1. one\n2. two"))
        #expect(sent.options.markupSpans.isEmpty)
    }

    @Test func sendingTrimsListWhitespaceAndKeepsTypedNumbers() throws {
        for (draft, body) in [("1. one\n2. ", "1. one\n2."), ("- a ", "- a"), ("- ", "-"), ("7. one\n3. two ", "7. one\n3. two")] {
            let sent = try #require(MessageComposer.compose(.init(text: draft)))
            #expect(sent.body == body)
            #expect(sent.options.markupSpans.isEmpty)
        }
    }

    @Test func sendingKeepsReferenceOffsetsThroughTheTrimmedEnd() throws {
        let jo = bare("jo@example.test")
        let lab = bare("lab@rooms.example.test")
        let references = DraftReferences(
            mentions: [.init(target: .user(jo), range: 4..<7)],
            roomLinks: [.init(room: lab, range: 8..<12)]
        )
        let sent = try #require(MessageComposer.compose(.init(text: "  - @Jo #lab \r\n ", references: references)))
        #expect(sent.body == "- @Jo #lab")
        #expect(sent.options.references == [
            Reference(kind: .mention, uri: "xmpp:jo@example.test", begin: 2, end: 5),
            Reference(kind: .data, uri: "xmpp:lab@rooms.example.test?join", begin: 6, end: 10),
        ])
    }

    @Test func listEditsAndInlineFormattingKeepMentionAndRoomLinkOffsets() throws {
        let text = "**👍** @Jo\n*hi* #lab"
        let jo = bare("jo@example.test")
        let lab = bare("lab@rooms.example.test")
        var recorded = RecordedDraftReferences(
            mentions: [.init(token: "@Jo", target: .user(jo))],
            roomLinks: [.init(room: lab, range: 15..<19)]
        )
        let edit = ComposerFormatting.apply(.orderedList, to: text, selection: nil)
        recorded.relocate(from: text, applying: edit.replacement)
        let sent = try #require(MessageComposer.compose(.init(text: edit.text, references: recorded.located(in: edit.text))))
        #expect(sent.body == "1. 👍 @Jo\n2. hi #lab")
        #expect(sent.options.markupSpans == [
            MarkupSpan(kind: .bold, start: 3, end: 4), MarkupSpan(kind: .italic, start: 12, end: 14),
        ])
        #expect(sent.options.references == [
            Reference(kind: .mention, uri: "xmpp:jo@example.test", begin: 5, end: 8),
            Reference(kind: .data, uri: "xmpp:lab@rooms.example.test?join", begin: 15, end: 19),
        ])
    }

    @Test func returnKeepsTheSurvivingOccurrenceOfAnIdenticalRoomLink() throws {
        let text = "1. #lab\n2. #lab"
        let first = bare("lab@rooms.example.test")
        let second = bare("lab@rooms.other.test")
        var recorded = RecordedDraftReferences(roomLinks: [
            RoomLinkDraft(room: first, range: 3..<7), RoomLinkDraft(room: second, range: 11..<15),
        ])
        guard case let .edit(edit) = ComposerReturn.decide(text: text, selection: 3..<11) else {
            Issue.record("Expected a list edit")
            return
        }
        recorded.relocate(from: text, applying: edit.replacement)
        let sent = try #require(MessageComposer.compose(.init(text: edit.text, references: recorded.located(in: edit.text))))
        #expect(sent.body == "1. \n2. #lab")
        #expect(sent.options.references == [
            Reference(kind: .data, uri: "xmpp:lab@rooms.other.test?join", begin: 7, end: 11),
        ])
    }
}
