import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

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

    @Test func mentionAfterAnEmptyFenceAndBlankLineStartsAtItsToken() {
        let result = ComposerMarkdown.compose("```\n```\n\n@Jo", references: .init(mentions: [mention("jo@w.s", 9..<12)]))
        #expect(result.body == "@Jo")
        #expect(result.references.mentions == [mention("jo@w.s", 0..<3)])
    }

    @Test func referencesUseScalarOffsetsAfterCRLFAndAFlagAndAreSortedByBegin() throws {
        let text = "```\r\n```\r\n\r\n🇳🇴 @Jo **#ops**"
        let result = try #require(MessageComposer.compose(Draft(
            text: text,
            references: .init(
                mentions: [mention("jo@w.s", 15..<18)],
                roomLinks: [RoomLinkDraft(room: room, range: 21..<25)]
            )
        )))
        #expect(result.body == "\r\n🇳🇴 @Jo #ops")
        #expect(result.options.references == [
            Reference(kind: .mention, uri: "xmpp:jo@w.s", begin: 5, end: 8),
            Reference(kind: .data, uri: "xmpp:ops@rooms.waddle.test?join", begin: 9, end: 13),
        ])
    }

    private let room = bare("ops@rooms.waddle.test")
    private let uri = "xmpp:ops@rooms.waddle.test?join"

    @Test func linkRangesUseScalarsAfterTrimmingMarkdownAndReplyFallback() throws {
        let text = "  **🎉e\u{301}** #ops "
        let link = RoomLinkDraft(room: room, range: 10..<14)
        let reply = ReplyContext(
            targetID: "parent", author: jid("peer@waddle.test"), parentBody: "🙂",
            parentAuthorName: "Peer"
        )
        let composed = try #require(MessageComposer.compose(Draft(
            text: text, references: .init(roomLinks: [link]), reply: reply, thread: "thread"
        )))
        #expect(composed.body == "> 🙂\n\n🎉e\u{301} #ops")
        #expect(composed.options.references == [Reference(kind: .data, uri: uri, begin: 9, end: 13)])
        #expect(composed.options.thread?.id == "thread")
    }

    @Test func identicalLabelsCanLinkToDifferentRooms() {
        let text = "#ops #ops"
        let links = [
            RoomLinkDraft(room: room, range: 0..<4),
            RoomLinkDraft(room: bare("ops@rooms.other.test"), range: 5..<9),
        ]
        let moved = RoomLinks.relocate(links, from: text, to: "**" + text + "**", replacing: nil)
        let composed = MessageComposer.compose(Draft(text: "**" + text + "**", references: .init(roomLinks: moved)))
        #expect(composed?.body == text)
        #expect(composed?.options.references == [
            Reference(kind: .data, uri: uri, begin: 0, end: 4),
            Reference(kind: .data, uri: "xmpp:ops@rooms.other.test?join", begin: 5, end: 9),
        ])
    }

    @Test func markdownPunctuationInAPickedLabelIsLiteral() {
        let text = "See #Team **Ops**."
        let link = RoomLinkDraft(room: room, range: 4..<17)
        let composed = MessageComposer.compose(Draft(text: text, references: .init(roomLinks: [link])))
        #expect(composed?.body == text)
        #expect(composed?.options.references == [Reference(kind: .data, uri: uri, begin: 4, end: 17)])
        #expect(composed?.options.markupSpans.isEmpty == true)
    }

    @Test(arguments: [
        ("**#Team **Ops****", "#Team **Ops**", Span(.bold, 0, 13)),
        ("~~#Team ~~Ops~~~~", "#Team ~~Ops~~", Span(.strikethrough, 0, 13)),
        ("*#Team *Ops**", "#Team *Ops*", Span(.italic, 0, 11)),
    ])
    private func formattingAroundAPickedLabelSkipsItsMarkers(text: String, label: String, span: Span) throws {
        let length = label.unicodeScalars.count
        let start = (text.unicodeScalars.count - length) / 2
        let link = RoomLinkDraft(room: room, range: start..<(start + length))
        let composed = try #require(MessageComposer.compose(Draft(text: text, references: .init(roomLinks: [link]))))
        #expect(composed.body == label)
        #expect(composed.options.markupSpans.map { Span($0.kind, $0.start, $0.end) } == [span])
        #expect(composed.options.references == [Reference(kind: .data, uri: uri, begin: 0, end: length)])
    }

    @Test(arguments: [
        ("**#Ops** later", 2..<8), ("#Ops** later**", 0..<6),
        ("`#Ops` later", 1..<6), ("#Ops` later`", 0..<5),
    ])
    func markersInPickedLabelsNeverPairWithOutsideMarkers(text: String, range: Range<Int>) throws {
        let link = RoomLinkDraft(room: room, range: range)
        let composed = try #require(MessageComposer.compose(Draft(text: text, references: .init(roomLinks: [link]))))
        #expect(composed.body == text)
        #expect(composed.options.markupSpans.isEmpty)
        #expect(composed.options.references == [Reference(kind: .data, uri: uri, begin: range.lowerBound, end: range.upperBound)])
    }

    @Test func roomLinkStartsAtItsLabelAfterAnEmptyCodeBlock() {
        let text = "```\n```\n\n#ops"
        let link = RoomLinkDraft(room: room, range: 9..<13)
        let composed = MessageComposer.compose(Draft(text: text, references: .init(roomLinks: [link])))
        #expect(composed?.body == "#ops")
        #expect(composed?.options.references == [Reference(kind: .data, uri: uri, begin: 0, end: 4)])
    }

    @Test func plainTextPassesThroughUntouched() {
        let result = ComposerMarkdown.compose("just words")
        #expect(result.body == "just words")
        #expect(result.spans.isEmpty)
    }

    @Test func inlineStylesStripMarkersAndEmitSpans() {
        let result = ComposerMarkdown.compose("**b** *i* ~~s~~ `c`")
        #expect(result.body == "b i s c")
        #expect(spans(result) == [
            Span(.bold, 0, 1),
            Span(.italic, 2, 3),
            Span(.strikethrough, 4, 5),
            Span(.code, 6, 7),
        ])
    }

    @Test func offsetsCountCodePointsAcrossEmoji() {
        let result = ComposerMarkdown.compose("😀 **bold**")
        #expect(result.body == "😀 bold")
        #expect(spans(result) == [Span(.bold, 2, 6)])
    }

    @Test func fencesBecomeCodeBlocksWithFenceLinesRemoved() {
        let result = ComposerMarkdown.compose("a\n```\ncode\n```\nb")
        #expect(result.body == "a\ncode\nb")
        #expect(spans(result) == [Span(.codeBlock, 2, 6)])
    }

    @Test func fenceLanguageInfoStringIsDropped() {
        let result = ComposerMarkdown.compose("```rust\nfn main() {}\n```")
        #expect(result.body == "fn main() {}")
        #expect(spans(result) == [Span(.codeBlock, 0, 12)])
    }

    @Test func unclosedFenceStaysLiteral() {
        let result = ComposerMarkdown.compose("```\nno closer")
        #expect(result.body == "```\nno closer")
        #expect(result.spans.isEmpty)
    }

    @Test func inlineMarkersInsideFencesAreLiteral() {
        let result = ComposerMarkdown.compose("```\n**not bold**\n```")
        #expect(result.body == "**not bold**")
        #expect(spans(result) == [Span(.codeBlock, 0, 12)])
    }

    @Test func inlineCodeProtectsItsContentFromOtherStyles() {
        let result = ComposerMarkdown.compose("`**x**`")
        #expect(result.body == "**x**")
        #expect(spans(result) == [Span(.code, 0, 5)])
    }

    @Test func quoteLinesEmitABlockquoteSpanKeepingMarkers() {
        let result = ComposerMarkdown.compose("> one\n> two")
        #expect(result.body == "> one\n> two")
        #expect(spans(result) == [Span(.blockquote, 0, 11)])
    }

    @Test func separateQuoteGroupsEmitSeparateSpans() {
        let result = ComposerMarkdown.compose("> a\nplain\n> b")
        #expect(spans(result) == [Span(.blockquote, 0, 3), Span(.blockquote, 10, 13)])
    }

    @Test func styledTextInsideAQuoteGetsBothSpans() {
        let result = ComposerMarkdown.compose("> **b**")
        #expect(result.body == "> b")
        #expect(spans(result) == [Span(.blockquote, 0, 3), Span(.bold, 2, 3)])
    }

    @Test func listsAreNotMarkdown() {
        let result = ComposerMarkdown.compose("- item\n1. other")
        #expect(result.body == "- item\n1. other")
        #expect(result.spans.isEmpty)
    }

    @Test func loneAsterisksAroundSpacesStayLiteral() {
        let result = ComposerMarkdown.compose("a * b * c")
        #expect(result.body == "a * b * c")
        #expect(result.spans.isEmpty)
    }

    @Test func mentionsRebaseAcrossRemovedMarkers() {
        let result = ComposerMarkdown.compose("**b** @alice", references: .init(mentions: [mention("alice@w.s", 6..<12)]))
        #expect(result.body == "b @alice")
        #expect(result.references.mentions == [mention("alice@w.s", 2..<8)])
    }

    @Test func mentionsRebaseAcrossRemovedFenceLines() {
        let result = ComposerMarkdown.compose("```\ncode\n```\n@alice hi", references: .init(mentions: [mention("alice@w.s", 13..<19)]))
        #expect(result.body == "code\n@alice hi")
        #expect(result.references.mentions == [mention("alice@w.s", 5..<11)])
    }

    @Test func mentionUntouchedWhenNoMarkdownPresent() {
        let original = MentionDraft(target: .everyone, range: 0..<2)
        let result = ComposerMarkdown.compose("@a hi", references: .init(mentions: [original]))
        #expect(result.references.mentions == [original])
    }

    @Test func tripleAsterisksResolveToBoldWithLiteralInnerMarkers() {
        // Marker styles do not nest: the outer ** pair wins, the inner
        // asterisks stay literal.
        let result = ComposerMarkdown.compose("***x***")
        #expect(result.body == "*x*")
        #expect(spans(result) == [Span(.bold, 0, 2)])
    }

    @Test func multipleBoldRunsKeepIndependentOffsets() {
        let result = ComposerMarkdown.compose("**a** mid **b**")
        #expect(result.body == "a mid b")
        #expect(spans(result) == [Span(.bold, 0, 1), Span(.bold, 6, 7)])
    }

    // MARK: - Scalar offsets

    @Test func astralEmojiCountsAsOneScalar() {
        let result = ComposerMarkdown.compose("👋 **hi**")
        #expect(result.body == "👋 hi")
        #expect(spans(result) == [Span(.bold, 2, 4)])
    }

    @Test func multiScalarGraphemesCountEveryScalar() {
        // 👍🏽 is one grapheme but two scalars; 👨‍👩‍👧 is five scalars.
        let result = ComposerMarkdown.compose("👍🏽👨‍👩‍👧 *x*")
        #expect(result.body == "👍🏽👨‍👩‍👧 x")
        #expect(spans(result) == [Span(.italic, 8, 9)])
    }

    @Test func emojiInsideStyledContentExtendsSpanByScalars() {
        let result = ComposerMarkdown.compose("~~🎉🎉~~ `👍🏽`")
        #expect(result.body == "🎉🎉 👍🏽")
        #expect(spans(result) == [Span(.strikethrough, 0, 2), Span(.code, 3, 5)])
    }

    @Test func mentionAfterEmojiRebasesAcrossRemovedBoldMarkers() {
        // Raw: "🦆 **hey** @bob" — @bob spans scalars 10..<14.
        let result = ComposerMarkdown.compose("🦆 **hey** @bob", references: .init(mentions: [mention("bob@w.s", 10..<14)]))
        #expect(result.body == "🦆 hey @bob")
        #expect(result.references.mentions == [mention("bob@w.s", 6..<10)])
        #expect(spans(result) == [Span(.bold, 2, 5)])
    }

    // MARK: - Mentions

    @Test func mentionInsideBoldRebasesAcrossOpeningMarker() {
        let result = ComposerMarkdown.compose("**@alice**", references: .init(mentions: [mention("alice@w.s", 2..<8)]))
        #expect(result.body == "@alice")
        #expect(result.references.mentions == [mention("alice@w.s", 0..<6)])
    }

    @Test func mentionCoveringOnlyRemovedMarkersIsDropped() {
        let result = ComposerMarkdown.compose("**b**", references: .init(mentions: [mention("alice@w.s", 0..<2)]))
        #expect(result.references.mentions.isEmpty)
    }

    // MARK: - Regex-parity edges

    @Test func lazyStrikeStopsAtFirstCloser() {
        let result = ComposerMarkdown.compose("~~a~~ b ~~c~~")
        #expect(result.body == "a b c")
        #expect(spans(result) == [Span(.strikethrough, 0, 1), Span(.strikethrough, 4, 5)])
    }

    @Test func italicMayEndOnAnAsterisk() {
        // `\*(\S(?:[^*\n]*?\S)??)\*` at 0: `\S` = "a", closer at 2.
        let result = ComposerMarkdown.compose("*a**")
        #expect(result.body == "a*")
        #expect(spans(result) == [Span(.italic, 0, 1)])
    }

    @Test func stylesDoNotCrossLines() {
        let result = ComposerMarkdown.compose("**a\nb** `c\nd`")
        #expect(result.body == "**a\nb** `c\nd`")
        #expect(result.spans.isEmpty)
    }

    @Test func blockquoteInsideCodeBlockIsLiteral() {
        let result = ComposerMarkdown.compose("```\n> not a quote\n```\n> quote")
        #expect(result.body == "> not a quote\n> quote")
        #expect(spans(result) == [Span(.codeBlock, 0, 13), Span(.blockquote, 14, 21)])
    }

    @Test func closingFenceMayCarrySurroundingWhitespace() {
        let result = ComposerMarkdown.compose("```\nx\n  ```  \ny")
        #expect(result.body == "x\ny")
        #expect(spans(result) == [Span(.codeBlock, 0, 1)])
    }
}
