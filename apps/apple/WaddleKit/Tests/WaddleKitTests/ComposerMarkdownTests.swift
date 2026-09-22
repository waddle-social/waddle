import Foundation
import Testing
@testable import WaddleKit

struct ComposerMarkdownTests {
    private struct Span: Equatable {
        let kind: MarkupSpan.Kind
        let start: Int
        let end: Int
        init(_ kind: MarkupSpan.Kind, _ start: Int, _ end: Int) {
            self.kind = kind
            self.start = start
            self.end = end
        }
    }

    private func spans(_ result: ComposedMarkdown) -> [Span] {
        result.spans.map { Span($0.kind, $0.start, $0.end) }
    }

    private func mention(_ jid: String, _ range: Range<Int>) -> MentionDraft {
        MentionDraft(target: .user(BareJID(parsing: jid)!), range: range)
    }

    @Test func plainTextPassesThroughUntouched() {
        let result = ComposerMarkdown.compose("just words", mentions: [])
        #expect(result.body == "just words")
        #expect(result.spans.isEmpty)
    }

    @Test func inlineStylesStripMarkersAndEmitSpans() {
        let result = ComposerMarkdown.compose("**b** *i* ~~s~~ `c`", mentions: [])
        #expect(result.body == "b i s c")
        #expect(spans(result) == [
            Span(.bold, 0, 1),
            Span(.italic, 2, 3),
            Span(.strikethrough, 4, 5),
            Span(.code, 6, 7),
        ])
    }

    @Test func offsetsCountCodePointsAcrossEmoji() {
        let result = ComposerMarkdown.compose("😀 **bold**", mentions: [])
        #expect(result.body == "😀 bold")
        #expect(spans(result) == [Span(.bold, 2, 6)])
    }

    @Test func fencesBecomeCodeBlocksWithFenceLinesRemoved() {
        let result = ComposerMarkdown.compose("a\n```\ncode\n```\nb", mentions: [])
        #expect(result.body == "a\ncode\nb")
        #expect(spans(result) == [Span(.codeBlock, 2, 6)])
    }

    @Test func fenceLanguageInfoStringIsDropped() {
        let result = ComposerMarkdown.compose("```rust\nfn main() {}\n```", mentions: [])
        #expect(result.body == "fn main() {}")
        #expect(spans(result) == [Span(.codeBlock, 0, 12)])
    }

    @Test func unclosedFenceStaysLiteral() {
        let result = ComposerMarkdown.compose("```\nno closer", mentions: [])
        #expect(result.body == "```\nno closer")
        #expect(result.spans.isEmpty)
    }

    @Test func inlineMarkersInsideFencesAreLiteral() {
        let result = ComposerMarkdown.compose("```\n**not bold**\n```", mentions: [])
        #expect(result.body == "**not bold**")
        #expect(spans(result) == [Span(.codeBlock, 0, 12)])
    }

    @Test func inlineCodeProtectsItsContentFromOtherStyles() {
        let result = ComposerMarkdown.compose("`**x**`", mentions: [])
        #expect(result.body == "**x**")
        #expect(spans(result) == [Span(.code, 0, 5)])
    }

    @Test func quoteLinesEmitABlockquoteSpanKeepingMarkers() {
        let result = ComposerMarkdown.compose("> one\n> two", mentions: [])
        #expect(result.body == "> one\n> two")
        #expect(spans(result) == [Span(.blockquote, 0, 11)])
    }

    @Test func separateQuoteGroupsEmitSeparateSpans() {
        let result = ComposerMarkdown.compose("> a\nplain\n> b", mentions: [])
        #expect(spans(result) == [Span(.blockquote, 0, 3), Span(.blockquote, 10, 13)])
    }

    @Test func styledTextInsideAQuoteGetsBothSpans() {
        let result = ComposerMarkdown.compose("> **b**", mentions: [])
        #expect(result.body == "> b")
        #expect(spans(result) == [Span(.blockquote, 0, 3), Span(.bold, 2, 3)])
    }

    @Test func listsAreNotMarkdown() {
        let result = ComposerMarkdown.compose("- item\n1. other", mentions: [])
        #expect(result.body == "- item\n1. other")
        #expect(result.spans.isEmpty)
    }

    @Test func loneAsterisksAroundSpacesStayLiteral() {
        let result = ComposerMarkdown.compose("a * b * c", mentions: [])
        #expect(result.body == "a * b * c")
        #expect(result.spans.isEmpty)
    }

    @Test func mentionsRebaseAcrossRemovedMarkers() {
        let result = ComposerMarkdown.compose("**b** @alice", mentions: [mention("alice@w.s", 6..<12)])
        #expect(result.body == "b @alice")
        #expect(result.mentions == [mention("alice@w.s", 2..<8)])
    }

    @Test func mentionsRebaseAcrossRemovedFenceLines() {
        let result = ComposerMarkdown.compose("```\ncode\n```\n@alice hi", mentions: [mention("alice@w.s", 13..<19)])
        #expect(result.body == "code\n@alice hi")
        #expect(result.mentions == [mention("alice@w.s", 5..<11)])
    }

    @Test func mentionUntouchedWhenNoMarkdownPresent() {
        let original = MentionDraft(target: .everyone, range: 0..<2)
        let result = ComposerMarkdown.compose("@a hi", mentions: [original])
        #expect(result.mentions == [original])
    }

    @Test func tripleAsterisksResolveToBoldWithLiteralInnerMarkers() {
        // Marker styles do not nest: the outer ** pair wins, the inner
        // asterisks stay literal.
        let result = ComposerMarkdown.compose("***x***", mentions: [])
        #expect(result.body == "*x*")
        #expect(spans(result) == [Span(.bold, 0, 2)])
    }

    @Test func multipleBoldRunsKeepIndependentOffsets() {
        let result = ComposerMarkdown.compose("**a** mid **b**", mentions: [])
        #expect(result.body == "a mid b")
        #expect(spans(result) == [Span(.bold, 0, 1), Span(.bold, 6, 7)])
    }

    // MARK: - Scalar offsets

    @Test func astralEmojiCountsAsOneScalar() {
        let result = ComposerMarkdown.compose("👋 **hi**", mentions: [])
        #expect(result.body == "👋 hi")
        #expect(spans(result) == [Span(.bold, 2, 4)])
    }

    @Test func multiScalarGraphemesCountEveryScalar() {
        // 👍🏽 is one grapheme but two scalars; 👨‍👩‍👧 is five scalars.
        let result = ComposerMarkdown.compose("👍🏽👨‍👩‍👧 *x*", mentions: [])
        #expect(result.body == "👍🏽👨‍👩‍👧 x")
        #expect(spans(result) == [Span(.italic, 8, 9)])
    }

    @Test func emojiInsideStyledContentExtendsSpanByScalars() {
        let result = ComposerMarkdown.compose("~~🎉🎉~~ `👍🏽`", mentions: [])
        #expect(result.body == "🎉🎉 👍🏽")
        #expect(spans(result) == [Span(.strikethrough, 0, 2), Span(.code, 3, 5)])
    }

    @Test func mentionAfterEmojiRebasesAcrossRemovedBoldMarkers() {
        // Raw: "🦆 **hey** @bob" — @bob spans scalars 10..<14.
        let result = ComposerMarkdown.compose("🦆 **hey** @bob", mentions: [mention("bob@w.s", 10..<14)])
        #expect(result.body == "🦆 hey @bob")
        #expect(result.mentions == [mention("bob@w.s", 6..<10)])
        #expect(spans(result) == [Span(.bold, 2, 5)])
    }

    // MARK: - Mentions

    @Test func mentionInsideBoldRebasesAcrossOpeningMarker() {
        let result = ComposerMarkdown.compose("**@alice**", mentions: [mention("alice@w.s", 2..<8)])
        #expect(result.body == "@alice")
        #expect(result.mentions == [mention("alice@w.s", 0..<6)])
    }

    @Test func mentionCoveringOnlyRemovedMarkersIsDropped() {
        let result = ComposerMarkdown.compose("**b**", mentions: [mention("alice@w.s", 0..<2)])
        #expect(result.mentions.isEmpty)
    }

    @Test func broadcastTargetsRenderLiteralURIs() {
        #expect(MentionTarget.everyone.uri == "xmpp:@everyone")
        #expect(MentionTarget.here.uri == "xmpp:@here")
        #expect(MentionTarget.user(BareJID(parsing: "Alice@W.S")!).uri == "xmpp:alice@w.s")
    }

    // MARK: - Regex-parity edges

    @Test func lazyStrikeStopsAtFirstCloser() {
        let result = ComposerMarkdown.compose("~~a~~ b ~~c~~", mentions: [])
        #expect(result.body == "a b c")
        #expect(spans(result) == [Span(.strikethrough, 0, 1), Span(.strikethrough, 4, 5)])
    }

    @Test func italicMayEndOnAnAsterisk() {
        // `\*(\S(?:[^*\n]*?\S)??)\*` at 0: `\S` = "a", closer at 2.
        let result = ComposerMarkdown.compose("*a**", mentions: [])
        #expect(result.body == "a*")
        #expect(spans(result) == [Span(.italic, 0, 1)])
    }

    @Test func stylesDoNotCrossLines() {
        let result = ComposerMarkdown.compose("**a\nb** `c\nd`", mentions: [])
        #expect(result.body == "**a\nb** `c\nd`")
        #expect(result.spans.isEmpty)
    }

    @Test func blockquoteInsideCodeBlockIsLiteral() {
        let result = ComposerMarkdown.compose("```\n> not a quote\n```\n> quote", mentions: [])
        #expect(result.body == "> not a quote\n> quote")
        #expect(spans(result) == [Span(.codeBlock, 0, 13), Span(.blockquote, 14, 21)])
    }

    @Test func closingFenceMayCarrySurroundingWhitespace() {
        let result = ComposerMarkdown.compose("```\nx\n  ```  \ny", mentions: [])
        #expect(result.body == "x\ny")
        #expect(spans(result) == [Span(.codeBlock, 0, 1)])
    }
}
