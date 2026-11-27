/// The actions shared by the native selection menus. A nil presentation means
/// there is no editable text selection to format.
public struct ComposerSelectionFormatting: Sendable {
    public enum Action: Hashable, Sendable {
        case format(ComposerFormat)
        case link
    }

    public static let orderedActions: [Action] = ComposerFormat.allCases.map(Action.format) + [.link]

    public let activeFormats: Set<ComposerFormat>

    public static func inDraft(
        _ text: String, selection: ComposerScalarRange?, roomLinks: [RoomLinkDraft] = []
    ) -> Self? {
        guard let selection, !selection.isEmpty, selection.lowerBound >= 0 else { return nil }
        let scalars = Array(text.unicodeScalars)
        guard selection.upperBound <= scalars.count,
              scalars[selection].contains(where: { !$0.properties.isWhitespace })
        else { return nil }

        let draft = ComposerDraftLines(scalars)
        let touched = draft.touched(by: selection)
        if ComposerBlockFormatting.exactFence(in: draft, selection: selection) != nil || draft.codeBlocks.contains(where: {
            $0.contentLines.contains(touched.lowerBound) && $0.contentLines.contains(touched.upperBound)
        }) {
            return Self(activeFormats: [.codeBlock])
        }

        let lists = ComposerListLines(draft)
        var active: Set<ComposerFormat> = []
        if touched.allSatisfy({ !lists.fenced.contains($0) && ComposerBlockFormatting.quoteMarker(in: draft, at: $0) != nil }) {
            active.insert(.quote)
        }
        if let kind = lists.lines[touched.lowerBound].item?.kind,
           touched.allSatisfy({ lists.lines[$0].item?.kind == kind }) {
            active.insert(kind == .ordered ? .orderedList : .bulletList)
        }
        active.formUnion(ComposerInlineFormatting.active(selection, in: draft, roomLinks: roomLinks))
        return Self(activeFormats: active)
    }
}
