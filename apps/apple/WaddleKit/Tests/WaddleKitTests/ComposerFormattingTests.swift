import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

struct ComposerFormattingTests {
    private func apply(_ format: ComposerFormat, _ text: String, _ selection: Range<Int>?) -> ComposerTextEdit {
        let edit = ComposerFormatting.apply(format, to: text, selection: selection)
        // Compare the visible draft and selection. The reference test below
        // exercises the full replacement, including the marker changes.
        return ComposerTextEdit(text: edit.text, selection: edit.selection)
    }

    @Test func inlineStylesWrapTheSelectionAndKeepItSelected() {
        #expect(apply(.bold, "say hi now", 4..<6) == ComposerTextEdit(text: "say **hi** now", selection: 6..<8))
        #expect(apply(.italic, "say hi now", 4..<6) == ComposerTextEdit(text: "say *hi* now", selection: 5..<7))
        #expect(apply(.strikethrough, "say hi now", 4..<6) == ComposerTextEdit(text: "say ~~hi~~ now", selection: 6..<8))
        #expect(apply(.code, "say hi now", 4..<6) == ComposerTextEdit(text: "say `hi` now", selection: 5..<7))
    }

    @Test func emptySelectionInsertsMarkerPairAroundTheCaret() {
        #expect(apply(.bold, "ab", 1..<1) == ComposerTextEdit(text: "a****b", selection: 3..<3))
    }

    @Test func unknownSelectionWrapsTheDraftWithoutItsSurroundingWhitespace() {
        #expect(apply(.bold, "hello ", nil) == ComposerTextEdit(text: "**hello** ", selection: 2..<7))
        #expect(apply(.code, "", nil) == ComposerTextEdit(text: "``", selection: 1..<1))
    }

    @Test func wrappedTextComposesToTheStyle() {
        let edit = apply(.bold, "say hi", 4..<6)
        let composed = ComposerMarkdown.compose(edit.text)
        #expect(composed.body == "say hi")
        #expect(composed.spans == [MarkupSpan(kind: .bold, start: 4, end: 6)])
    }

    @Test func offsetsAreUnicodeScalars() {
        // "é" (U+00E9) and 👍 are one scalar each.
        #expect(apply(.italic, "é👍x", 1..<2) == ComposerTextEdit(text: "é*👍*x", selection: 2..<3))
    }

    @Test func outOfRangeSelectionIsClamped() {
        #expect(apply(.code, "ab", 5..<9) == ComposerTextEdit(text: "ab``", selection: 3..<3))
    }

    @Test func codeBlockFencesTheSelectionOnItsOwnLines() {
        #expect(apply(.codeBlock, "run ls now", 4..<6) == ComposerTextEdit(text: "run \n```\nls\n```\n now", selection: 9..<11))
        #expect(apply(.codeBlock, "ls", 0..<2) == ComposerTextEdit(text: "```\nls\n```", selection: 4..<6))
        #expect(apply(.codeBlock, "", nil) == ComposerTextEdit(text: "```\n\n```", selection: 4..<4))
    }

    @Test func codeBlockComposesToACodeBlock() {
        let edit = apply(.codeBlock, "ls -la", 0..<6)
        let composed = ComposerMarkdown.compose(edit.text)
        #expect(composed.body == "ls -la")
        #expect(composed.spans == [MarkupSpan(kind: .codeBlock, start: 0, end: 6)])
    }

    @Test func quotePrefixesEveryTouchedLine() {
        #expect(apply(.quote, "a\nbc\nd", 3..<4) == ComposerTextEdit(text: "a\n> bc\nd", selection: 5..<6))
        #expect(apply(.quote, "a\nbc\nd", 0..<4) == ComposerTextEdit(text: "> a\n> bc\nd", selection: 2..<8))
        // A selection ending right after a newline leaves the next line alone.
        #expect(apply(.quote, "a\nb", 0..<2) == ComposerTextEdit(text: "> a\nb", selection: 2..<4))
    }

    @Test func quoteWithoutSelectionQuotesEveryLine() {
        #expect(apply(.quote, "one\ntwo", nil) == ComposerTextEdit(text: "> one\n> two", selection: 2..<11))
        #expect(apply(.quote, "", nil) == ComposerTextEdit(text: "> ", selection: 2..<2))
    }

    @Test func quotePreservesCRLFAndExcludesTheFollowingLine() {
        let edit = apply(.quote, "a\r\nbc\r\nd", 0..<7)
        #expect(edit.text == "> a\r\n> bc\r\nd")
        #expect(edit.selection == 2..<11)
    }

    @Test func nativeFormattingInsertsMarkersWithoutReplacingTheSelection() {
        let draft = "say hi now"
        let change = ComposerTextReplacement.applyingFormat(.bold, to: draft, selection: 4..<6)
        #expect(change.replacement.nativeChanges(from: draft) == [
            ComposerDraftChange(6..<6, "**"),
            ComposerDraftChange(4..<4, "**"),
        ])
        #expect(change.edit.selection == 6..<8)
    }

    @Test(arguments: [
        (ComposerFormat.bold, "**abc**", 2..<5),
        (.italic, "*abc*", 1..<4),
        (.strikethrough, "~~abc~~", 2..<5),
        (.code, "`abc`", 1..<4),
        (.quote, "> abc", 2..<5),
        (.codeBlock, "```\nabc\n```", 4..<7),
        (.orderedList, "1. abc", 3..<6),
        (.bulletList, "- abc", 2..<5),
    ])
    func formatCanBeAppliedRemovedAndReapplied(format: ComposerFormat, formatted: String, inner: Range<Int>) {
        let on = apply(format, "abc", 0..<3)
        #expect(on.text == formatted)
        #expect(on.selection == inner)

        let off = apply(format, on.text, on.selection)
        #expect(off.text == "abc")
        #expect(off.selection == 0..<3)

        let includingMarkers = apply(format, formatted, 0..<formatted.unicodeScalars.count)
        #expect(includingMarkers.text == "abc")
        #expect(includingMarkers.selection == 0..<3)

        let again = apply(format, off.text, off.selection)
        #expect(again.text == formatted)
        #expect(again.selection == inner)
    }

    @Test(arguments: [ComposerFormat.bold, .italic, .strikethrough, .code])
    func pressingAgainInsideAnEmptyPairRestoresTheCaret(format: ComposerFormat) {
        let on = apply(format, "ab", 1..<1)
        #expect(apply(format, on.text, on.selection) == ComposerTextEdit(text: "ab", selection: 1..<1))
    }

    @Test func aBoldDelimiterIsNotAnEmptyItalicPair() {
        #expect(apply(.italic, "**b**", 1..<1) == ComposerTextEdit(text: "****b**", selection: 2..<2))
    }

    @Test func quoteRemovesOneLevelOnlyWhenAllTouchedLinesAreQuoted() {
        let off = apply(.quote, "> > a\r\n> bc\r\nd", 4..<13)
        #expect(off.text == "> a\r\nbc\r\nd")
        #expect(off.selection == 2..<9)
        #expect(apply(.quote, ">a\r\nb", 0..<5).text == "> >a\r\n> b")
        #expect(apply(.quote, ">a", 1..<2).text == "a")
    }

    @Test(arguments: [ComposerFormat.quote, .codeBlock, .orderedList, .bulletList])
    func lineFormattingRoundTripsCRLF(format: ComposerFormat) {
        let original = "top\r\na\r\nbc\r\nend"
        let selection = 5..<10
        let on = apply(format, original, selection)
        if format == .codeBlock {
            #expect(on.text == "top\r\n```\na\r\nbc\n```\n\r\nend")
            let sent = ComposerMarkdown.compose(on.text)
            #expect(sent.body == "top\r\na\r\nbc\n\r\nend")
            #expect(sent.spans == [MarkupSpan(kind: .codeBlock, start: 5, end: 10)])
        } else {
            #expect(!on.text.replacingOccurrences(of: "\r\n", with: "").contains("\n"))
        }
        let off = apply(format, on.text, on.selection)
        #expect(off.text == original)
        #expect(off.selection == selection)
        #expect(apply(format, off.text, off.selection) == on)
    }

    @Test func unfencingPreservesSurroundingLineBreaksAndSelectsOnlyTheCode() {
        let formatted = "top\r\n```swift\r\nabc\r\n```\r\nend"
        for selection in [15..<18, 5..<23, 5..<25] {
            #expect(apply(.codeBlock, formatted, selection) == ComposerTextEdit(text: "top\r\nabc\r\nend", selection: 5..<8))
        }
        #expect(apply(.codeBlock, "```\nabc\n```", 5..<6).text == "```\na\n```\nb\n```\nc\n```")
    }

    @Test func unfencingAMidlineWrapKeepsTheCodeOnItsOwnLine() {
        let on = apply(.codeBlock, "run ls now", 4..<6)
        let off = apply(.codeBlock, on.text, on.selection)
        #expect(off == ComposerTextEdit(text: "run \nls\n now", selection: 5..<7))
        #expect(apply(.codeBlock, off.text, off.selection) == on)
    }

    @Test(arguments: ComposerFormat.allCases)
    func togglesRelocateMentionsAndRepeatedRoomLinkOccurrences(format: ComposerFormat) {
        let first = bare("lab@rooms.example.test")
        let second = bare("lab@rooms.other.test")
        let original = "hi @Jo #lab now\r\n#lab"
        var recorded = RecordedDraftReferences(
            mentions: [.init(token: "@Jo", target: .user(bare("jo@example.test")))],
            roomLinks: [.init(room: first, range: 7..<11), .init(room: second, range: 17..<21)]
        )
        // Inline formats style the whole first line; line formats only touch that line.
        let on = ComposerFormatting.apply(format, to: original, selection: 0..<15)
        recorded.relocate(from: original, applying: on.replacement)
        let located = recorded.located(in: on.text)
        #expect(located.mentions.map { String(on.text.unicodeScalars.dropFirst($0.range.lowerBound).prefix($0.range.count)) } == ["@Jo"])
        #expect(located.roomLinks.map(\.room) == [first, second])
        #expect(located.roomLinks.map { String(on.text.unicodeScalars.dropFirst($0.range.lowerBound).prefix($0.range.count)) } == ["#lab", "#lab"])

        let off = ComposerFormatting.apply(format, to: on.text, selection: on.selection)
        recorded.relocate(from: on.text, applying: off.replacement)
        #expect(off.text == original)
        #expect(recorded.located(in: off.text).mentions.map(\.range) == [3..<6])
        #expect(recorded.roomLinks == [.init(room: first, range: 7..<11), .init(room: second, range: 17..<21)])
    }

    @Test func unwrappingInlineCodeRestoresAMentionOnItsMarkerBoundary() {
        let mention = RecordedMention(token: "@Jo", target: .user(bare("jo@example.test")))
        var recorded = RecordedDraftReferences(mentions: [mention])
        let on = ComposerFormatting.apply(.code, to: "@Jo", selection: 0..<3)
        recorded.relocate(from: "@Jo", applying: on.replacement)
        #expect(recorded.mentions == [mention])
        // Preserve the existing send-time token boundaries for formatted code.
        #expect(recorded.located(in: on.text).mentions.isEmpty)
        let off = ComposerFormatting.apply(.code, to: on.text, selection: on.selection)
        recorded.relocate(from: on.text, applying: off.replacement)
        #expect(recorded.located(in: off.text).mentions == [.init(target: mention.target, range: 0..<3)])
    }

    @Test func nativeBoldToggleRemovesOnlyTheMarkersAroundALiteralRoomLabel() {
        let draft = "**#Team **Ops****"
        let link = RoomLinkDraft(room: bare("ops@rooms.waddle.test"), range: 2..<15)
        let edit = ComposerFormatting.apply(.bold, to: draft, selection: link.range, roomLinks: [link])
        let changes = edit.replacement.nativeChanges(from: draft)

        #expect(edit.text == "#Team **Ops**")
        #expect(edit.selection == 0..<13)
        #expect(changes.allSatisfy { $0.text.isEmpty })
        #expect(changes.flatMap { Array($0.range) }.sorted() == [0, 1, 15, 16])
        #expect(RoomLinks.relocate([link], from: draft, applying: edit.replacement) == [
            RoomLinkDraft(room: link.room, range: 0..<13),
        ])
    }
}

struct ComposerInsertionTests {
    @Test func nativeInsertionKeepsTheReplacementRangeWhenTextIsRepeated() {
        let draft = "🎉🎉"
        let change = ComposerTextReplacement.replacingSelection(with: "🎉", in: draft, selection: 0..<1)
        #expect(change.replacement.nativeChanges(from: draft) == [ComposerDraftChange(0..<1, "🎉")])
        #expect(change.edit.selection == 1..<1)
    }

    @Test func replacingSelectionPutsTheCaretAfterTheInsertion() {
        #expect(ComposerInsertion.replacingSelection(with: "🎉", in: "hi there", selection: 3..<8)
            == ComposerTextEdit(text: "hi 🎉", selection: 4..<4))
        #expect(ComposerInsertion.replacingSelection(with: "🎉", in: "hi", selection: nil)
            == ComposerTextEdit(text: "hi🎉", selection: 3..<3))
    }

    @Test func appendingTokenAddsASpaceOnlyWhenNeeded() {
        #expect(ComposerInsertion.appendingToken("@", to: "") == "@")
        #expect(ComposerInsertion.appendingToken("@", to: "hi") == "hi @")
        #expect(ComposerInsertion.appendingToken("@", to: "hi ") == "hi @")
        #expect(ComposerInsertion.appendingToken("@", to: "hi\n") == "hi\n@")
    }

    @Test func appendingAtSignOpensTheEmptyMentionList() {
        let text = ComposerInsertion.appendingToken("@", to: "hey")
        #expect(ComposerSuggestionTrigger.find(in: text)?.kind == .mention)
        #expect(ComposerSuggestionTrigger.find(in: text)?.query == "")
    }

    @Test func linksArePaddedWithSpaces() throws {
        let url = try #require(URL(string: "https://waddle.social"))
        #expect(ComposerInsertion.insertingLink(url, into: "see", selection: nil)
            == ComposerTextEdit(text: "see https://waddle.social ", selection: 26..<26))
        #expect(ComposerInsertion.insertingLink(url, into: "see docs", selection: 4..<8)
            == ComposerTextEdit(text: "see docs https://waddle.social ", selection: 31..<31))
        #expect(ComposerInsertion.insertingLink(url, into: "a b", selection: 2..<2)
            == ComposerTextEdit(text: "a https://waddle.social b", selection: 24..<24))
    }

    @Test func linkURLAcceptsOnlyHTTP() {
        #expect(ComposerInsertion.linkURL(from: " waddle.social/x ")?.absoluteString == "https://waddle.social/x")
        #expect(ComposerInsertion.linkURL(from: "http://example.com")?.absoluteString == "http://example.com")
        #expect(ComposerInsertion.linkURL(from: "") == nil)
        #expect(ComposerInsertion.linkURL(from: "two words") == nil)
        #expect(ComposerInsertion.linkURL(from: "javascript://alert(1)") == nil)
        #expect(ComposerInsertion.linkURL(from: "ftp://example.com") == nil)
    }
}
