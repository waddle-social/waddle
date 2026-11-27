/// List toggles preserve existing text and replace only list markers.
enum ComposerLists {
    static func toggle(
        _ kind: ComposerListLines.Kind, in scalars: Scalars, selection: ComposerScalarRange
    ) -> ComposerTextEdit {
        let draft = ComposerDraftLines(scalars)
        let touched = draft.touched(by: selection)
        if let code = draft.codeBlocks.first(where: {
            ($0.opening...$0.closing).contains(touched.lowerBound) && ($0.opening...$0.closing).contains(touched.upperBound)
        }) {
            // Tiptap turns the whole code node into one item, including embedded newlines.
            let first = draft.lines[code.opening].content.lowerBound
            let last = draft.lines[code.closing].content.upperBound
            if code.closing == code.opening + 1 {
                return draft.edit([ComposerDraftChange(first..<last, kind.marker())], selection: selection)
            }
            let content = draft.lines[code.opening + 1].content.lowerBound
            let end = draft.lines[code.closing - 1].content.upperBound
            return draft.edit(
                [ComposerDraftChange(first..<content, kind.marker()), ComposerDraftChange(end..<last, "")],
                selection: selection
            )
        }

        let lists = ComposerListLines(draft)
        let selected = touched.filter { !lists.fenced.contains($0) }
        guard let last = selected.last else { return draft.edit([], selection: selection) }
        if selected.allSatisfy({ lists.lines[$0].item?.kind == kind }) {
            let changes = selected.compactMap { lists.changeMarker(at: $0, to: "") }
            return draft.edit(changes + lists.renumber(after: last), selection: selection)
        }
        return draft.edit(wrap(selected, in: lists, kind: kind), selection: selection)
    }

    private static func wrap(
        _ selected: [ComposerLineIndex], in lists: ComposerListLines, kind: ComposerListLines.Kind
    ) -> [ComposerDraftChange] {
        var included = Set<ComposerLineIndex>()
        for index in selected where included.insert(index).inserted {
            // A kind change applies to the enclosing run of the original list.
            guard let item = lists.lines[index].item, item.kind != kind else { continue }
            included.formUnion(lists.run(from: index, step: -1))
            included.formUnion(lists.run(from: index, step: 1))
        }

        let draft = lists.draft
        var previous: ComposerLineIndex?
        var number = "1"
        return included.sorted().compactMap { index in
            let prefix = draft.scalars[lists.prefix(at: index)]
            if let previous, previous + 1 == index, prefix.elementsEqual(draft.scalars[lists.prefix(at: previous)]) {
                number = ComposerListLines.nextNumber(after: number)
            } else if index > 0, let prior = lists.lines[index - 1].item?.number,
                      prefix.elementsEqual(draft.scalars[lists.prefix(at: index - 1)]) {
                number = ComposerListLines.nextNumber(after: prior)
            } else {
                number = lists.lines[index].item?.number ?? "1"
            }
            previous = index
            return lists.changeMarker(at: index, to: kind.marker(number))
        }
    }
}
