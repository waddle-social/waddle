import Testing
@testable import WaddleKit

struct ComposerInlineFormattingTests {
    struct RunSplit {
        let text: String
        let selection: Range<Int>
        let remaining: [Range<Int>]
    }

    private static let formats: [(ComposerFormat, MarkupSpan.Kind)] = [
        (.bold, .bold), (.italic, .italic), (.strikethrough, .strikethrough), (.code, .code),
    ]
    private static let testMention = MentionTarget.user(BareJID(parsing: "user@example.org")!)

    private static let drafts = [
        "a", "**a**", "~~a~~", "*a*", "`a`", "**hello world**", "**a b c**", "*hello world*",
        "2 * 3 * 4", "x ** y", "a* b *c", "* item", "***abc***", "`**a**`", "**abc**",
        "**helloworld**", "*helloworld*", "**a****b**", "*a\nb*", "a\nb", "**a** ~~b~~",
        "```\nabc\n```", "a\n```\n```", "> **a**", "**😀e\u{301}**", "**a**\r\n`b`",
    ]

    @Test(arguments: [
        ("say hello now", 4..<10, "say ", "hello", " now", 4),
        ("say hello now", 3..<9, "say ", "hello", " now", 4),
        ("hello world ", 0..<12, "", "hello world", " ", 0),
        ("one\ntwo", 0..<4, "", "one", "\ntwo", 0),
        ("  hello  ", 0..<9, "  ", "hello", "  ", 0),
    ], [
        (ComposerFormat.bold, MarkupSpan.Kind.bold, "**"), (.italic, .italic, "*"),
        (.strikethrough, .strikethrough, "~~"), (.code, .code, "`"),
    ])
    func trimsSelectionEdgesForEveryInlineFormat(
        example: (String, Range<Int>, String, String, String, Int), style: (ComposerFormat, MarkupSpan.Kind, String)
    ) {
        let (text, selection, prefix, selected, suffix, sentStart) = example
        let (format, kind, marker) = style
        let start = prefix.unicodeScalars.count + marker.unicodeScalars.count
        let sent = compose(text)
        let original = ComposerTextEdit(text: text, selection: selection)
        #expect(sent.spans.isEmpty)
        let on = check(format, original, draft: prefix + marker + selected + marker + suffix,
                       selection: start..<(start + selected.count), body: sent.body,
                       spans: [MarkupSpan(kind: kind, start: sentStart, end: sentStart + selected.count)])
        #expect(active(on.text, on.selection).contains(format))
        check(format, on, draft: text,
              selection: prefix.count..<(prefix.count + selected.count), body: sent.body, spans: [])
    }

    @Test func whitespaceOnlySelectionsDoNothing() {
        let text = "one \n two"
        let selection = 3..<6
        #expect(active(text, selection).isEmpty)
        for (format, _) in Self.formats {
            #expect(ComposerFormatting.apply(format, to: text, selection: selection) ==
                    ComposerTextEdit(text: text, selection: selection))
        }
    }

    @Test func aCaretInsideAnEditorDelimiterStillInsertsAnEmptyPair() {
        let text = " ```\n**a**\n```"
        let edit = ComposerFormatting.apply(.italic, to: text, selection: 6..<6)
        #expect(edit.text == " ```\n****a**\n```")
        #expect(edit.selection == 7..<7)
    }

    @Test(arguments: [
        (ComposerFormat.bold, .strikethrough, "**a**", 2..<3, MarkupSpan.Kind.bold),
        (.strikethrough, .bold, "~~a~~", 2..<3, .strikethrough),
        (.italic, .strikethrough, "*a*", 1..<2, .italic),
        (.bold, .code, "**a**", 2..<3, .bold),
    ] as [(ComposerFormat, ComposerFormat, String, Range<Int>, MarkupSpan.Kind)])
    func anotherFormatCannotReplaceOrNestInsideTheExistingSentSpan(
        first: ComposerFormat, second: ComposerFormat, formatted: String, selection: Range<Int>, kind: MarkupSpan.Kind
    ) {
        let original = ComposerTextEdit(text: "a", selection: 0..<1)
        let on = check(first, original, draft: formatted, selection: selection, body: "a", spans: [
            MarkupSpan(kind: kind, start: 0, end: 1),
        ])
        #expect(ComposerMarkdown.compose(original.text).spans.isEmpty)
        let blocked = check(second, on, draft: formatted, selection: selection, body: "a", spans: [
            MarkupSpan(kind: kind, start: 0, end: 1),
        ])
        check(first, blocked, draft: "a", selection: 0..<1, body: "a", spans: [])
    }

    @Test(arguments: [
        (ComposerFormat.bold, "**abc**", 3..<4, "abc", MarkupSpan.Kind.bold,
         RunSplit(text: "**a**b**c**", selection: 5..<6, remaining: [0..<1, 2..<3])),
        (.bold, "**helloworld**", 2..<7, "helloworld", .bold,
         RunSplit(text: "hello**world**", selection: 0..<5, remaining: [5..<10])),
        (.italic, "*helloworld*", 1..<6, "helloworld", .italic,
         RunSplit(text: "hello*world*", selection: 0..<5, remaining: [5..<10])),
        (.bold, "***abc***", 3..<6, "*abc*", .bold,
         RunSplit(text: "*****abc*", selection: 5..<8, remaining: [0..<1])),
    ])
    func splittingAndRejoiningARepresentableRun(
        format: ComposerFormat, text: String, selection: Range<Int>, body: String, kind: MarkupSpan.Kind, split: RunSplit
    ) {
        let before = ComposerMarkdown.compose(text)
        #expect(before.body == body)
        let end = text == "***abc***" ? 4 : body.unicodeScalars.count
        #expect(before.spans == [MarkupSpan(kind: kind, start: 0, end: end)])
        let off = check(format, ComposerTextEdit(text: text, selection: selection), draft: split.text, selection: split.selection,
                        body: body, spans: split.remaining.map { MarkupSpan(kind: kind, start: $0.lowerBound, end: $0.upperBound) })
        check(format, off, draft: text, selection: selection, body: body, spans: before.spans)
    }

    @Test(arguments: [
        (ComposerFormat.bold, MarkupSpan.Kind.bold, "**hello world**", 2..<7, "hello world",
         RunSplit(text: "hello **world**", selection: 0..<5, remaining: [6..<11])),
        (.bold, .bold, "**a b c**", 4..<5, "a b c",
         RunSplit(text: "**a** b **c**", selection: 6..<7, remaining: [0..<1, 4..<5])),
        (.italic, .italic, "*hello world*", 1..<6, "hello world",
         RunSplit(text: "hello *world*", selection: 0..<5, remaining: [6..<11])),
    ])
    func removesFormatFromAWordAndLeavesTheAdjacentSpaceOutsideTheRemainingRuns(
        format: ComposerFormat, kind: MarkupSpan.Kind, text: String, selection: Range<Int>, body: String, split: RunSplit
    ) {
        #expect(compose(text).spans == [MarkupSpan(kind: kind, start: 0, end: body.count)])
        #expect(active(text, selection).contains(format))
        let off = check(format, ComposerTextEdit(text: text, selection: selection), draft: split.text, selection: split.selection,
                        body: body, spans: split.remaining.map { MarkupSpan(kind: kind, start: $0.lowerBound, end: $0.upperBound) })
        #expect(!active(off.text, off.selection).contains(format))
    }

    @Test(arguments: [
        ("**hello** world", "hello world", [0..<5]),
        ("some **bold** and more", "some bold and more", [5..<9]),
        ("ab**cd**ef", "abcdef", [2..<4]),
    ])
    func boldAppliesToTheWholeMixedSelection(text: String, body: String, previous: [Range<Int>]) {
        #expect(compose(text).spans == previous.map { MarkupSpan(kind: .bold, start: $0.lowerBound, end: $0.upperBound) })
        let selection = 0..<text.unicodeScalars.count
        #expect(!active(text, selection).contains(.bold))
        let on = check(.bold, ComposerTextEdit(text: text, selection: selection), draft: "**" + body + "**",
                       selection: 2..<(body.count + 2), body: body, spans: [MarkupSpan(kind: .bold, start: 0, end: body.count)])
        #expect(active(on.text, on.selection).contains(.bold))
        check(.bold, on, draft: body, selection: 0..<body.count, body: body, spans: [])
    }

    @Test(arguments: [
        (ComposerFormat.bold, "**alpha**  **beta**", "alpha  beta"),
        (.italic, "*alpha*  *beta*", "alpha  beta"),
    ])
    func removesTheStyleWhenEverySelectedNonSpaceCharacterAlreadyHasIt(
        format: ComposerFormat, text: String, body: String
    ) {
        let selection = 0..<text.unicodeScalars.count
        #expect(active(text, selection).contains(format))
        check(format, ComposerTextEdit(text: text, selection: selection),
              draft: body, selection: 0..<body.count, body: body, spans: [])
    }

    @Test(arguments: [
        (ComposerFormat.bold, "x ** y", 2..<4, "x ** y", []),
        (.italic, "***abc***", 3..<6, "*abc*", [MarkupSpan(kind: .bold, start: 0, end: 4)]),
        (.bold, "`**a**`", 3..<4, "**a**", [MarkupSpan(kind: .code, start: 0, end: 5)]),
        (.italic, "`**a**`", 3..<4, "**a**", [MarkupSpan(kind: .code, start: 0, end: 5)]),
        (.bold, "one\ntwo", 0..<7, "one\ntwo", []),
        (.italic, "one\ntwo", 0..<7, "one\ntwo", []),
        (.bold, "  ```\n**ops**\n```", 8..<11, "**ops**", [MarkupSpan(kind: .codeBlock, start: 0, end: 7)]),
    ] as [(ComposerFormat, String, Range<Int>, String, [MarkupSpan])])
    func leavesUnrepresentableTogglesUntouched(
        format: ComposerFormat, text: String, selection: Range<Int>, body: String, spans: [MarkupSpan]
    ) {
        let before = compose(text)
        #expect(before.body == body)
        #expect(before.spans == spans)
        #expect(!active(text, selection).contains(format))
        check(format, ComposerTextEdit(text: text, selection: selection), draft: text, selection: selection, body: body, spans: spans)
    }

    @Test func retainsLiteralStarsWhenTrimmingASelection() {
        let spans = [MarkupSpan(kind: .italic, start: 4, end: 5)]
        let on = check(.italic, ComposerTextEdit(text: "2 * 3 * 4", selection: 3..<6),
                       draft: "2 * *3* * 4", selection: 5..<6, body: "2 * 3 * 4", spans: spans)
        check(.italic, on, draft: "2 * 3 * 4", selection: 4..<5, body: "2 * 3 * 4", spans: [])
    }

    @Test func italicRoundTripsAWholeStarListItem() {
        let text = "* item"
        let original = ComposerTextEdit(text: text, selection: 0..<6)
        #expect(ComposerMarkdown.compose(text).spans.isEmpty)
        let on = check(.italic, original, draft: "** item*", selection: 1..<7, body: text, spans: [
            MarkupSpan(kind: .italic, start: 0, end: 6),
        ])
        check(.italic, on, draft: text, selection: 0..<6, body: text, spans: [])
    }

    @Test func markersInsideAPickedRoomLabelStayLiteralThroughFormatting() throws {
        let text = "#Team **Ops**"
        var references = RecordedDraftReferences(roomLinks: [
            .init(room: try #require(BareJID(parsing: "ops@rooms.waddle.test")), range: 0..<13),
        ])
        let labelSelection = 8..<11
        #expect(active(text, labelSelection) == [.bold])
        let before = ComposerMarkdown.compose(text, references: references.located(in: text))
        #expect(before.body == text)
        #expect(before.spans.isEmpty)
        #expect(ComposerSelectionFormatting.inDraft(text, selection: labelSelection, roomLinks: references.roomLinks)?.activeFormats.isEmpty == true)
        let blocked = ComposerFormatting.apply(.bold, to: text, selection: labelSelection, roomLinks: references.roomLinks)
        #expect(blocked == ComposerTextEdit(text: text, selection: labelSelection))

        let on = ComposerFormatting.apply(.strikethrough, to: text, selection: 0..<13, roomLinks: references.roomLinks)
        #expect(on.text == "~~#Team **Ops**~~")
        #expect(on.selection == 2..<15)
        references.relocate(from: text, applying: on.replacement)
        let styled = ComposerMarkdown.compose(on.text, references: references.located(in: on.text))
        #expect(styled.body == text)
        #expect(styled.spans == [MarkupSpan(kind: .strikethrough, start: 0, end: 13)])
        #expect(styled.references == before.references)

        let off = ComposerFormatting.apply(.strikethrough, to: on.text, selection: on.selection, roomLinks: references.roomLinks)
        #expect(off.text == text)
        #expect(off.selection == 0..<13)
        references.relocate(from: on.text, applying: off.replacement)
        #expect(ComposerMarkdown.compose(off.text, references: references.located(in: off.text)) == before)
    }

    @Test func removingStrikeKeepsSentWhitespaceAtTheEdgesOfTheDraft() {
        let text = "~~ a ~~"
        let before = compose(text)
        #expect(before.body == " a ")
        #expect(before.spans == [MarkupSpan(kind: .strikethrough, start: 0, end: 3)])
        let off = check(.strikethrough, ComposerTextEdit(text: text, selection: 2..<5),
                        draft: "~~ ~~a~~ ~~", selection: 5..<6, body: before.body, spans: [
                            MarkupSpan(kind: .strikethrough, start: 0, end: 1),
                            MarkupSpan(kind: .strikethrough, start: 2, end: 3),
                        ])
        check(.strikethrough, off, draft: text, selection: 3..<4, body: before.body, spans: before.spans)
    }

    @Test func trailingEmptyFenceDoesNotAddACharacterToTheSentSelection() {
        let text = "a\n```\n```"
        let on = check(.bold, ComposerTextEdit(text: text, selection: 0..<9),
                       draft: "**a**\n```\n```", selection: 2..<3, body: "a",
                       spans: [MarkupSpan(kind: .bold, start: 0, end: 1)])
        #expect(active(on.text, 2..<6) == [.bold])
        check(.bold, ComposerTextEdit(text: on.text, selection: 2..<6),
              draft: text, selection: 0..<1, body: "a", spans: [])
    }

    @Test func everyNonemptySelectionEitherTogglesAsSentOrLeavesTheDraftAlone() {
        var seed: UInt64 = 2638
        let alphabet = Array("ab *~`")
        func generateDraft(length: Int) -> String {
            String((0..<length).map { _ in
                seed = seed &* 6_364_136_223_846_793_005 &+ 1
                return alphabet[Int((seed >> 32) % UInt64(alphabet.count))]
            })
        }
        let generated = (1...12).flatMap { length in
            (0..<16).map { _ in generateDraft(length: length) }
        }
        var cases = 0
        var noOps = 0
        for text in (Self.drafts + generated) {
            let count = text.unicodeScalars.count
            let before = compose(text)
            let body = Array(before.body.unicodeScalars)
            for lower in 0..<count {
                for upper in (lower + 1)...count {
                    let selection = lower..<upper
                    let sent = sentRange(selection, in: text)
                    let selected = contentRange(sent, in: body)
                    let highlighted = active(text, selection)
                    for (format, kind) in Self.formats {
                        cases += 1
                        let context = Comment(rawValue: "\(text.debugDescription) \(selection) \(format)")
                        let edit = ComposerFormatting.apply(format, to: text, selection: selection)
                        let wasAllStyled = !selected.isEmpty && selected.allSatisfy {
                            body[$0].properties.isWhitespace || covers(before.spans, kind, $0)
                        }
                        #expect(highlighted.contains(format) == wasAllStyled, context)
                        if edit.text == text {
                            noOps += 1
                            #expect(edit == ComposerTextEdit(text: text, selection: selection), context)
                            continue
                        }
                        let after = compose(edit.text)
                        #expect(after.body.unicodeScalars.elementsEqual(before.body.unicodeScalars), context)
                        #expect(!selected.isEmpty, context)
                        #expect(sentRange(edit.selection, in: edit.text) == selected, context)
                        #expect(after.spans.filter { $0.kind != kind } == before.spans.filter { $0.kind != kind }, context)
                        for change in edit.changes where !change.range.isEmpty {
                            #expect(sentRange(change.range, in: text).isEmpty, context)
                        }
                        for offset in body.indices where !body[offset].properties.isWhitespace {
                            let expected = selected.contains(offset) ? !wasAllStyled : covers(before.spans, kind, offset)
                            #expect(covers(after.spans, kind, offset) == expected, context)
                        }
                    }
                }
            }
        }
        #expect(cases == 27_544)
        print("Inline formatting no-ops: \(noOps)/\(cases) (\(100 * Double(noOps) / Double(cases))%)")
        // This marker-heavy corpus includes whitespace, conflicting styles, and literal delimiters.
        // The no-op rate fell from 66.72% to 58.15%. Allow less than two percentage points
        // of headroom so regressions toward the previous behavior fail this test.
        #expect(noOps * 100 < cases * 60)
    }

    @Test(arguments: ["a", " say hello now ", "hello world ", "one\ntwo", "\tone\r\n\n two\t", " a😀e\u{301} b ", "\u{00A0}word\u{2003}"])
    func plainSingleLineSelectionsAlwaysToggleAndRestoreTheSentSpans(text: String) {
        let before = compose(text)
        let scalars = Array(text.unicodeScalars)
        for lower in scalars.indices {
            for upper in (lower + 1)...scalars.count {
                let selection = lower..<upper
                let content = contentRange(selection, in: scalars)
                guard !content.isEmpty, !scalars[content].contains("\n") else { continue }
                for (format, kind) in Self.formats {
                    let context = Comment(rawValue: "\(text.debugDescription) \(selection) \(format)")
                    let on = ComposerFormatting.apply(format, to: text, selection: selection)
                    let sent = compose(on.text)
                    #expect(on.text != text, context)
                    #expect(active(on.text, on.selection).contains(format), context)
                    #expect(sent.body.unicodeScalars.elementsEqual(before.body.unicodeScalars), context)
                    let selected = sentRange(content, in: text)
                    #expect(sent.spans == [MarkupSpan(kind: kind, start: selected.lowerBound, end: selected.upperBound)], context)
                    let off = ComposerFormatting.apply(format, to: on.text, selection: on.selection)
                    #expect(off.text == text, context)
                    #expect(off.selection == content, context)
                    #expect(compose(off.text).spans == before.spans, context)
                }
            }
        }
    }

    private func active(_ text: String, _ selection: Range<Int>) -> Set<ComposerFormat> {
        ComposerSelectionFormatting.inDraft(text, selection: selection)?.activeFormats ?? []
    }

    private func compose(_ text: String) -> (body: String, spans: [MarkupSpan]) {
        let sent = MessageComposer.compose(Draft(text: text))
        return (sent?.body ?? "", sent?.options.markupSpans ?? [])
    }

    private func covers(_ spans: [MarkupSpan], _ kind: MarkupSpan.Kind, _ offset: Int) -> Bool {
        spans.contains { $0.kind == kind && ($0.start..<$0.end).contains(offset) }
    }

    private func contentRange(_ range: Range<Int>, in scalars: [Unicode.Scalar]) -> Range<Int> {
        let content = scalars[range].indices.filter { !scalars[$0].properties.isWhitespace }
        guard let first = content.first, let last = content.last else { return range.lowerBound..<range.lowerBound }
        return first..<(last + 1)
    }

    private func sentRange(_ selection: Range<Int>, in text: String) -> Range<Int> {
        let raw = Array(text.unicodeScalars)
        // Mention rebasing exposes the send parser's selection mapping. Find
        // each boundary with a reference to the full remaining draft suffix.
        func offset(_ point: Int) -> Int {
            let references = DraftReferences(mentions: [
                MentionDraft(target: Self.testMention, range: point..<raw.count),
            ])
            let sent = MessageComposer.compose(Draft(text: text, references: references))
            let count = sent?.body.unicodeScalars.count ?? 0
            return min(sent?.options.references.first?.begin ?? count, count)
        }
        return offset(selection.lowerBound)..<offset(selection.upperBound)
    }

    @discardableResult
    private func check(
        _ format: ComposerFormat, _ original: ComposerTextEdit, draft: String, selection: Range<Int>,
        body: String, spans: [MarkupSpan], sourceLocation: SourceLocation = #_sourceLocation
    ) -> ComposerTextEdit {
        let edit = ComposerFormatting.apply(format, to: original.text, selection: original.selection)
        #expect(edit.text == draft, sourceLocation: sourceLocation)
        #expect(edit.selection == selection, sourceLocation: sourceLocation)
        let sent = compose(edit.text)
        #expect(sent.body == body, sourceLocation: sourceLocation)
        #expect(sent.spans == spans, sourceLocation: sourceLocation)
        return edit
    }
}
