import Testing
@testable import WaddleKit

struct ComposerSelectionFormattingTests {
    @Test func offersTheWebActionsInOrderForSelectedText() throws {
        let menu = try #require(ComposerSelectionFormatting.inDraft("a👍b", selection: 1..<2))
        #expect(ComposerSelectionFormatting.orderedActions == [
            .format(.bold), .format(.italic), .format(.strikethrough), .format(.orderedList),
            .format(.bulletList), .format(.quote), .format(.code), .format(.codeBlock), .link,
        ])
        #expect(menu.activeFormats.isEmpty)
    }

    @Test func requiresAnInBoundsNonblankSelection() {
        #expect(ComposerSelectionFormatting.inDraft("a", selection: nil) == nil)
        #expect(ComposerSelectionFormatting.inDraft("a", selection: 1..<1) == nil)
        #expect(ComposerSelectionFormatting.inDraft("a", selection: 0..<2) == nil)
        #expect(ComposerSelectionFormatting.inDraft("a", selection: -1..<1) == nil)
        #expect(ComposerSelectionFormatting.inDraft("a \t\r\n\u{00A0}b", selection: 1..<6) == nil)
        #expect(ComposerSelectionFormatting.inDraft(" a ", selection: 0..<3) != nil)
    }

    @Test func highlightsAnInlineMarkThatCoversTheSelection() {
        #expect(active("**a👍c**", 3..<4) == [.bold])
        #expect(active("*abc*", 1..<4) == [.italic])
        #expect(active("~~abc~~", 2..<5) == [.strikethrough])
        #expect(active("`*abc*`", 2..<5) == [.code])
        #expect(active("`*abc*`", 1..<6) == [.code])
        #expect(active("**abc** plain", 2..<13).isEmpty)
        #expect(active("**abc", 2..<5).isEmpty)
    }

    @Test func highlightsSendSpansEvenWhenRemovalWouldChangeTheSentText() {
        #expect(active("**a *b* c**", 5..<6) == [.bold])
        #expect(active("**a *b* c**", 4..<7) == [.bold])
        #expect(active("*a **b** c*", 5..<6).isEmpty)
        #expect(active("***abc***", 3..<6) == [.bold])
        #expect(active("**abc**", 0..<7) == [.bold])
        #expect(active("** a **", 2..<5) == [.italic])
        #expect(active("*a\nb*", 1..<4).isEmpty)
        #expect(active("```\nabc\n```", 0..<11) == [.codeBlock])
    }

    @Test func highlightsBlockFormatsAcrossTheTouchedLines() {
        #expect(active("> 1. a\n> 2. b\nplain", 5..<14) == [.quote, .orderedList])
        #expect(active(">quote", 1..<6) == [.quote])
        #expect(active("- a\n+ b", 2..<7) == [.bulletList])
        #expect(active("- a\n1. b", 2..<8).isEmpty)
        #expect(active("```\n> - *a*\n```", 9..<10) == [.codeBlock])
        #expect(active("- **ab**", 4..<6) == [.bulletList, .bold])
    }

    private func active(_ text: String, _ selection: ComposerScalarRange) -> Set<ComposerFormat> {
        ComposerSelectionFormatting.inDraft(text, selection: selection)?.activeFormats ?? []
    }
}
