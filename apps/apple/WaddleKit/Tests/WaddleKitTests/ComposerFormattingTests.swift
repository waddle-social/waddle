import Foundation
import Testing
@testable import WaddleKit

struct ComposerFormattingTests {
    private func apply(_ format: ComposerFormat, _ text: String, _ selection: Range<Int>?) -> ComposerTextEdit {
        ComposerFormatting.apply(format, to: text, selection: selection)
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

    @Test func unknownSelectionAppendsAtTheEnd() {
        #expect(apply(.bold, "hello ", nil) == ComposerTextEdit(text: "hello ****", selection: 8..<8))
        #expect(apply(.code, "", nil) == ComposerTextEdit(text: "``", selection: 1..<1))
    }

    @Test func wrappedTextComposesToTheStyle() {
        let edit = apply(.bold, "say hi", 4..<6)
        let composed = ComposerMarkdown.compose(edit.text, mentions: [])
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
        let composed = ComposerMarkdown.compose(edit.text, mentions: [])
        #expect(composed.body == "ls -la")
        #expect(composed.spans == [MarkupSpan(kind: .codeBlock, start: 0, end: 6)])
    }

    @Test func quotePrefixesEveryTouchedLine() {
        #expect(apply(.quote, "a\nbc\nd", 3..<4) == ComposerTextEdit(text: "a\n> bc\nd", selection: 5..<6))
        #expect(apply(.quote, "a\nbc\nd", 0..<4) == ComposerTextEdit(text: "> a\n> bc\nd", selection: 2..<8))
        // A selection ending right after a newline leaves the next line alone.
        #expect(apply(.quote, "a\nb", 0..<2) == ComposerTextEdit(text: "> a\nb", selection: 2..<4))
    }

    @Test func quoteWithoutSelectionQuotesTheLastLine() {
        #expect(apply(.quote, "one\ntwo", nil) == ComposerTextEdit(text: "one\n> two", selection: 9..<9))
        #expect(apply(.quote, "", nil) == ComposerTextEdit(text: "> ", selection: 2..<2))
    }
}

struct ComposerInsertionTests {
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

    @Test func appendedMentionTokenStartsAMentionQuery() {
        let text = ComposerInsertion.appendingToken("@", to: "hey")
        #expect(MentionTokens.trailingQuery(in: text)?.text == "")
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
