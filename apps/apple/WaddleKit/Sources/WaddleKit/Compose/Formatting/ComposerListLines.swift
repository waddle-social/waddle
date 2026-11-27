import Foundation

/// List markers and their literal prefixes in the plain-text draft.
struct ComposerListLines {
    enum Kind {
        case bullet, ordered

        func marker(_ number: String = "1") -> String {
            self == .bullet ? "- " : "\(number). "
        }
    }

    struct Item {
        let number: String?
        let marker: ComposerScalarRange

        var kind: Kind { number == nil ? .bullet : .ordered }
    }

    struct Line {
        let quoteEnd: Int
        let item: Item?
    }

    let draft: ComposerDraftLines
    let lines: [Line]
    let fenced: IndexSet

    init(_ draft: ComposerDraftLines) {
        self.draft = draft
        var fenced = IndexSet()
        for block in draft.codeBlocks {
            fenced.insert(integersIn: block.opening..<(block.closing + 1))
        }
        self.fenced = fenced
        lines = draft.lines.enumerated().map { index, line in
            let end = line.content.upperBound
            let text = draft.scalars
            var quoteEnd = line.content.lowerBound
            while quoteEnd + 1 < end, text[quoteEnd] == ">", text[quoteEnd + 1] == " " { quoteEnd += 2 }
            var start = quoteEnd
            while start < end, text[start] == " " { start += 1 }
            return Line(quoteEnd: quoteEnd, item: fenced.contains(index) ? nil : Self.item(in: draft, at: start, end: end))
        }
    }

    private static func item(in draft: ComposerDraftLines, at start: Int, end: Int) -> Item? {
        let text = draft.scalars
        guard start + 1 < end else { return nil }
        if text[start + 1] == " ", ["-", "*", "+"].contains(text[start]) {
            return Item(number: nil, marker: start..<(start + 2))
        }
        var dot = start
        while dot < end, (48...57).contains(text[dot].value) { dot += 1 }
        guard dot > start, dot + 1 < end, text[dot] == ".", text[dot + 1] == " " else { return nil }
        let significant = text[start..<dot].firstIndex { $0 != "0" } ?? (dot - 1)
        return Item(number: draft.text(in: significant..<dot), marker: start..<(dot + 2))
    }

    /// Increment the marker's decimal digits so every generated marker still parses.
    static func nextNumber(after number: String) -> String {
        var digits = Array(number.utf8)
        guard let last = digits.lastIndex(where: { $0 != 57 }) else {
            return "1" + String(repeating: "0", count: digits.count)
        }
        digits[last] += 1
        for index in (last + 1)..<digits.count { digits[index] = 48 }
        return String(decoding: digits, as: UTF8.self)
    }

    func prefix(at index: ComposerLineIndex) -> ComposerScalarRange {
        let line = lines[index]
        return draft.lines[index].content.lowerBound..<(line.item?.marker.lowerBound ?? line.quoteEnd)
    }

    func changeMarker(at index: ComposerLineIndex, to marker: String) -> ComposerDraftChange? {
        let range = lines[index].item?.marker ?? lines[index].quoteEnd..<lines[index].quoteEnd
        return draft.text(in: range) == marker ? nil : ComposerDraftChange(range, marker)
    }

    func continues(after index: ComposerLineIndex) -> Bool {
        let next = index + 1
        guard lines.indices.contains(next), let item = lines[index].item, let following = lines[next].item,
              draft.scalars[prefix(at: index)].elementsEqual(draft.scalars[prefix(at: next)])
        else { return false }
        return item.number.map(Self.nextNumber(after:)) == following.number
    }

    func run(from index: ComposerLineIndex, step: Int) -> [ComposerLineIndex] {
        var run: [ComposerLineIndex] = []
        var next = index + step
        while lines.indices.contains(next), continues(after: step > 0 ? next - 1 : next) {
            run.append(next)
            next += step
        }
        return run
    }

    func renumber(after index: ComposerLineIndex, startingAt number: String = "1") -> [ComposerDraftChange] {
        guard lines[index].item?.kind == .ordered else { return [] }
        var number = number
        return run(from: index, step: 1).compactMap { line in
            defer { number = Self.nextNumber(after: number) }
            return changeMarker(at: line, to: Kind.ordered.marker(number))
        }
    }
}
