import Testing
@testable import WaddleKit

struct ComposerReturnTests {
    @Test(arguments: ComposerReturnKey.Keyboard.allCases)
    func markedTextKeepsReturnWithTheInputMethod(_ keyboard: ComposerReturnKey.Keyboard) {
        let action = ComposerReturnKey.route(text: "- item", selection: nil, hasMarkedText: true, keyboard: keyboard) {
            Issue.record("The input method still owns Return")
            return true
        }
        #expect(action == .useNative)
    }

    @Test func suggestionTakesReturnBeforeTheList() {
        let hardware = ComposerReturnKey.route(text: "- @al", selection: nil, hasMarkedText: false, keyboard: .hardware) { true }
        #expect(hardware == .suggestion)
        let onScreen = ComposerReturnKey.route(text: "- @al", selection: nil, hasMarkedText: false, keyboard: .onScreen) {
            Issue.record("On-screen Return must leave the suggestion for a tap")
            return true
        }
        guard case let .edit(edit) = onScreen else {
            Issue.record("Expected on-screen Return to continue the list")
            return
        }
        #expect(edit.text == "- @al\n- ")
        #expect(edit.selection == 8..<8)
        #expect(ComposerReturnKey.route(text: "hi @al", selection: nil, hasMarkedText: false, keyboard: .onScreen) { true }
            == .useNative)
    }

    @Test(arguments: ComposerReturnKey.Keyboard.allCases)
    func bothKeyboardsContinueTheListAndOnlyHardwareSubmits(_ keyboard: ComposerReturnKey.Keyboard) {
        let list = ComposerReturnKey.route(text: "- item", selection: nil, hasMarkedText: false, keyboard: keyboard) { false }
        if case let .edit(edit) = list {
            #expect(edit.text == "- item\n- ")
            #expect(edit.selection == 9..<9)
        } else {
            Issue.record("Expected Return to continue the list")
        }
        let plain = ComposerReturnKey.route(text: "hi", selection: nil, hasMarkedText: false, keyboard: keyboard) { false }
        #expect(plain == (keyboard == .hardware ? .submit : .useNative))
    }

    @Test func plainTextLeavesReturnToTheField() {
        #expect(ComposerReturn.decide(text: "plain", selection: 2..<2) == .useDefault)
        #expect(ComposerReturn.decide(text: "", selection: nil) == .useDefault)
    }

    @Test func splitsAnItemAtTheCaret() {
        expectEdit("- one two", 5..<5, "- one\n-  two", 8)
    }

    @Test func deletesASelectionWithinAnItemThenSplits() {
        expectEdit("- one two", 3..<6, "- o\n- two", 6)
    }

    @Test func deletesASelectionSpanningItemsThenSplits() {
        expectEdit("- ab\n- cd\n- ef", 3..<8, "- a\n- d\n- ef", 6)
    }

    @Test func aSelectionFromPlainTextIntoAListRestartsTheRemainingList() {
        expectEdit("hello\n1. ab\n2. cd", 2..<10, "he\nb\n1. cd", 3)
    }

    @Test func splittingOrderedItemsRenumbersFollowingSiblings() {
        expectEdit("1. a\n2. bc\n3. d", 9..<9, "1. a\n2. b\n3. c\n4. d", 13)
        expectEdit("8. alpha\n9. bravo\n10. charlie", 14..<14, "8. alpha\n9. br\n10. avo\n11. charlie", 19)
    }

    @Test func splittingStopsRenumberingAtTheFirstLineOutsideTheRun() {
        expectEdit("1. a\n2024. was a good year\n2. b", 4..<4, "1. a\n2. \n2024. was a good year\n2. b", 8)
        expectEdit("1. a\n5. b", 4..<4, "1. a\n2. \n5. b", 8)
        expectEdit("1. a\n2. b\n\n1. c\n2. d", 4..<4, "1. a\n2. \n3. b\n\n1. c\n2. d", 8)
        expectEdit("1. a\n- b\n2. c", 4..<4, "1. a\n2. \n- b\n2. c", 8)
    }

    @Test func aGeneratedOrderedMarkerStillExitsAtTheIntegerBoundary() {
        let text = "9223372036854775807. a"
        let continued = text + "\n9223372036854775808. "
        expectEdit(text, nil, continued, 44)
        expectEdit(continued, nil, text + "\n", 23)
    }

    @Test func liftingTheEmptyItemSplitsAnOrderedList() {
        expectEdit("1. a\n2. \n3. b\n4. c", 8..<8, "1. a\n\n1. b\n2. c", 5)
        expectEdit("1. a\n2. \n3. b\n5. c\n4. d", 8..<8, "1. a\n\n1. b\n5. c\n4. d", 5)
    }

    @Test func liftingTheLastEmptyItemLeavesAPlainLine() {
        expectEdit("- a\n- ", nil, "- a\n", 4)
    }

    @Test func whitespaceIsContentInTiptap() {
        expectEdit("-   ", nil, "-   \n- ", 7)
    }

    @Test func splittingAnIndentedItemRepeatsItsPrefix() {
        expectEdit("1. parent\n   - child\n2. next", 20..<20, "1. parent\n   - child\n   - \n2. next", 26)
    }

    @Test func leavingAnEmptyIndentedItemKeepsItsSpaces() {
        expectEdit("1. parent\n   - \n2. next", 15..<15, "1. parent\n   \n2. next", 13)
    }

    @Test func aDifferentIndentationEndsTheRun() {
        expectEdit("9. abc\n   10. different indent\n10. next", 5..<5,
                   "9. ab\n10. c\n   10. different indent\n10. next", 10)
    }

    @Test func tabsAreNotListIndentation() {
        #expect(ComposerReturn.decide(text: "\t- item", selection: nil) == .useDefault)
    }

    @Test(arguments: ["*", "+"])
    func continuesAndExitsOtherBulletMarkers(_ marker: String) {
        expectEdit("\(marker) a", nil, "\(marker) a\n\(marker) ", 6)
        expectEdit("\(marker) ", nil, "", 0)
    }

    @Test func continuesQuotedItemsInsideTheQuote() {
        expectEdit("> - a", nil, "> - a\n> - ", 10)
    }

    @Test func usesTheDraftsCRLFLineEnding() {
        expectEdit("- a\r\n- b", 3..<3, "- a\r\n- \r\n- b", 7)
        expectEdit("- a\r\n- b", 4..<4, "- a\r\n- \r\n- b", 7)
    }

    @Test func insideCodeReturnInsertsANewlineEvenOnTextThatLooksLikeAList() {
        expectEdit("```\n- abc\n```", 7..<7, "```\n- a\nbc\n```", 8)
    }

    @Test func codeReturnCanReplaceASelectionAcrossCodeLines() {
        expectEdit("```\nab\ncd\n```", 5..<8, "```\na\nd\n```", 6)
    }

    @Test func nativeReturnEditsDescribeEveryReplacementAgainstTheOriginalDraft() {
        expectNativeEdit("- one two", 3..<6, [
            ComposerDraftChange(3..<6, "\n- "),
        ], caret: 6)
        expectNativeEdit("```\nab\ncd\n```", 5..<8, [
            ComposerDraftChange(5..<8, "\n"),
        ], caret: 6)
        expectNativeEdit("8. a\n9. b\n10. c", 4..<4, [
            ComposerDraftChange(10..<14, "11. "),
            ComposerDraftChange(5..<8, "10. "),
            ComposerDraftChange(4..<4, "\n9. "),
        ], caret: 8)
    }

    @Test func selectionEndingInCodeSplitsTheSurvivingTextAsParagraphs() {
        expectEdit("head\n```\nabc\n```\ntail", 2..<11, "he\nc\ntail", 3)
    }

    @Test func selectionStartingInCodeSplitsTheSurvivingTextAsCode() {
        expectEdit("head\n```\nabc\n```\ntail", 10..<19, "head\n```\na\n```\n```\nil\n```", 19)
    }

    @Test func returnOnTheFinalBlankCodeLineMovesOutsideTheFence() {
        expectEdit("```\ncode\n\n```", 9..<9, "```\ncode\n```\n", 13)
        expectEdit("```\r\ncode\r\n\r\n```", 11..<11, "```\r\ncode\r\n```\r\n", 16)
    }

    @Test func onlyAnEmptyFinalLineAfterCodeExitsTheFence() {
        expectEdit("```\n\n```", 4..<4, "```\n\n\n```", 5)
        expectEdit("```\na\n \n```", 7..<7, "```\na\n \n\n```", 8)
    }

    private func expectNativeEdit(
        _ text: String, _ selection: ComposerScalarRange, _ changes: [ComposerDraftChange], caret: Int,
        sourceLocation: SourceLocation = #_sourceLocation
    ) {
        guard case let .edit(edit) = ComposerReturn.decide(text: text, selection: selection) else {
            Issue.record("Expected a Return edit", sourceLocation: sourceLocation)
            return
        }
        #expect(edit.replacement.nativeChanges(from: text) == changes, sourceLocation: sourceLocation)
        #expect(edit.selection == caret..<caret, sourceLocation: sourceLocation)
    }

    private func expectEdit(
        _ text: String, _ selection: ComposerScalarRange?, _ expected: String, _ caret: Int,
        sourceLocation: SourceLocation = #_sourceLocation
    ) {
        guard case let .edit(edit) = ComposerReturn.decide(text: text, selection: selection) else {
            Issue.record("Expected a Return edit", sourceLocation: sourceLocation)
            return
        }
        #expect(edit.text == expected, sourceLocation: sourceLocation)
        #expect(edit.selection == caret..<caret, sourceLocation: sourceLocation)
    }
}
