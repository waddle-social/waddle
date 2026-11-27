import Testing
@testable import WaddleKit

struct ComposerBubbleVisibilityTests {
    private let first = 2..<5
    private let second = 3..<8

    private func ready() -> ComposerBubbleVisibility {
        var bubble = ComposerBubbleVisibility()
        bubble.reduce(.focusChanged(true))
        bubble.reduce(.windowKeyChanged(true))
        return bubble
    }

    private func settled() -> ComposerBubbleVisibility {
        var bubble = ready()
        bubble.reduce(.selectionChanged(first))
        bubble.reduce(.pauseElapsed)
        return bubble
    }

    @Test func keyboardSelectionWaitsForPauseAndAChangedSelectionHidesTheOldOne() {
        var bubble = ready()
        bubble.reduce(.selectionChanged(first))
        #expect(bubble.pendingSelection == first)
        #expect(bubble.visibleSelection == nil)
        bubble.reduce(.pauseElapsed)
        #expect(bubble.visibleSelection == first)
        bubble.reduce(.selectionChanged(second))
        #expect(bubble.visibleSelection == nil)
        #expect(bubble.pendingSelection == second)
        bubble.reduce(.pauseElapsed)
        #expect(bubble.visibleSelection == second)
        bubble.reduce(.selectionChanged(nil))
        #expect(bubble.visibleSelection == nil)
        #expect(bubble.pendingSelection == nil)
    }

    @Test func mouseDragHidesTheBubbleUntilMouseUpAndThenShowsImmediately() {
        var bubble = settled()
        bubble.reduce(.mouseDown)
        bubble.reduce(.selectionChanged(second))
        bubble.reduce(.pauseElapsed)
        #expect(bubble.visibleSelection == nil)
        #expect(bubble.pendingSelection == nil)
        bubble.reduce(.mouseUp)
        #expect(bubble.visibleSelection == second)
    }

    @Test(arguments: [
        ComposerBubbleVisibility.Suppression.linkPrompt, .formattingBar, .suggestions,
    ])
    func closingASuppressorRestoresTheUnchangedSelection(suppressor: ComposerBubbleVisibility.Suppression) {
        var bubble = settled()
        bubble.reduce(.suppressionChanged(suppressor))
        #expect(bubble.visibleSelection == nil)
        bubble.reduce(.suppressionChanged([]))
        #expect(bubble.visibleSelection == first)
    }

    @Test func selectionCanSettleWhileSuppressedAndOnlyAppearsAfterTheLastBlockerCloses() {
        var bubble = ready()
        bubble.reduce(.suppressionChanged([.linkPrompt, .suggestions]))
        bubble.reduce(.selectionChanged(first))
        bubble.reduce(.pauseElapsed)
        bubble.reduce(.suppressionChanged(.linkPrompt))
        #expect(bubble.visibleSelection == nil)
        bubble.reduce(.windowKeyChanged(false))
        bubble.reduce(.suppressionChanged([]))
        #expect(bubble.visibleSelection == nil)
        bubble.reduce(.windowKeyChanged(true))
        #expect(bubble.visibleSelection == first)
    }

    @Test func focusKeyWindowAndMarkedTextIndependentlyHideTheUnchangedSelection() {
        var bubble = settled()
        bubble.reduce(.focusChanged(false))
        #expect(bubble.visibleSelection == nil)
        bubble.reduce(.windowKeyChanged(false))
        bubble.reduce(.focusChanged(true))
        #expect(bubble.visibleSelection == nil)
        bubble.reduce(.windowKeyChanged(true))
        #expect(bubble.visibleSelection == first)
        bubble.reduce(.markedTextChanged(true))
        #expect(bubble.visibleSelection == nil)
        bubble.reduce(.markedTextChanged(false))
        #expect(bubble.visibleSelection == first)
    }

    @Test(arguments: [ComposerBubbleVisibility.Event.typed, .escape])
    func typingAndEscapeInvalidateTheSelectionUntilItChanges(event: ComposerBubbleVisibility.Event) {
        var bubble = ready()
        bubble.reduce(.selectionChanged(first))
        bubble.reduce(event)
        bubble.reduce(.pauseElapsed)
        bubble.reduce(.selectionChanged(first))
        bubble.reduce(.windowKeyChanged(false))
        bubble.reduce(.windowKeyChanged(true))
        #expect(bubble.visibleSelection == nil)
        #expect(bubble.pendingSelection == nil)
        bubble.reduce(.selectionChanged(second))
        bubble.reduce(.pauseElapsed)
        #expect(bubble.visibleSelection == second)
    }

    @Test func formattingAfterEscapeKeepsTheBubbleDismissedUntilAnotherSelection() {
        var bubble = settled()
        bubble.reduce(.escape)
        bubble.reduce(.formatApplied(second))
        bubble.reduce(.selectionChanged(second))
        bubble.reduce(.pauseElapsed)
        bubble.reduce(.mouseDown)
        bubble.reduce(.mouseUp)
        #expect(bubble.visibleSelection == nil)
        #expect(bubble.pendingSelection == nil)
        bubble.reduce(.selectionChanged(first))
        bubble.reduce(.pauseElapsed)
        #expect(bubble.visibleSelection == first)
    }

    @Test func formatImmediatelyShowsItsResultingSelectionWhenEligible() {
        var bubble = settled()
        bubble.reduce(.suppressionChanged(.formattingBar))
        bubble.reduce(.formatApplied(second))
        #expect(bubble.visibleSelection == nil)
        #expect(bubble.pendingSelection == nil)
        bubble.reduce(.suppressionChanged([]))
        #expect(bubble.visibleSelection == second)
        bubble.reduce(.formatApplied(nil))
        #expect(bubble.visibleSelection == nil)
    }
}
