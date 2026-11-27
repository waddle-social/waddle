typealias ComposerLineIndex = Int

/// A scalar-indexed view of the draft shared by formatting and Return.
struct ComposerDraftLines {
    struct Line {
        let content: ComposerScalarRange
        let ending: ComposerScalarRange

        /// Send-time markup counts a retained CR as part of the line.
        var beforeLF: ComposerScalarRange {
            content.lowerBound..<(ending.isEmpty ? content.upperBound : ending.upperBound - 1)
        }
    }

    struct CodeBlock {
        let opening: ComposerLineIndex
        let closing: ComposerLineIndex

        var contentLines: Range<ComposerLineIndex> { (opening + 1)..<closing }
    }

    let scalars: Scalars
    let lines: [Line]
    let codeBlocks: [CodeBlock]

    init(_ scalars: Scalars) {
        self.scalars = scalars
        lines = Self.split(scalars)
        var blocks: [CodeBlock] = []
        var opening: ComposerLineIndex?
        for (index, line) in lines.enumerated() {
            if let start = opening {
                if closesMarkdownFence(scalars[line.content]) {
                    blocks.append(CodeBlock(opening: start, closing: index))
                    opening = nil
                }
            } else if opensMarkdownFence(scalars[line.content]) {
                opening = index
            }
        }
        codeBlocks = blocks
    }

    static func split(_ scalars: Scalars) -> [Line] {
        var lines: [Line] = []
        var start = 0
        for lf in scalars.indices where scalars[lf] == "\n" {
            let end = lf > start && scalars[lf - 1] == "\r" ? lf - 1 : lf
            lines.append(Line(content: start..<end, ending: end..<(lf + 1)))
            start = lf + 1
        }
        lines.append(Line(content: start..<scalars.endIndex, ending: scalars.endIndex..<scalars.endIndex))
        return lines
    }

    func line(at offset: Int) -> ComposerLineIndex {
        lines.lastIndex { $0.content.lowerBound <= offset } ?? 0
    }

    func touched(by range: ComposerScalarRange) -> ClosedRange<ComposerLineIndex> {
        line(at: range.lowerBound)...line(at: range.isEmpty ? range.upperBound : range.upperBound - 1)
    }

    func newline(at index: ComposerLineIndex) -> String {
        let ending = lines[index].ending
        if !ending.isEmpty { return text(in: ending) }
        if index > 0 { return text(in: lines[index - 1].ending) }
        return "\n"
    }

    func text(in range: ComposerScalarRange) -> String {
        String(String.UnicodeScalarView(scalars[range]))
    }

    func edit(_ changes: [ComposerDraftChange], selection: ComposerScalarRange) -> ComposerTextEdit {
        let changes = changes.sorted { $0.range.lowerBound < $1.range.lowerBound }
        var result = scalars
        for change in changes.reversed() {
            result.replaceSubrange(change.range, with: change.text.unicodeScalars)
        }
        return ComposerTextEdit(
            text: ComposerScalars.string(result),
            selection: map(selection.lowerBound, through: changes)..<map(selection.upperBound, through: changes),
            changes: changes
        )
    }

    /// A point in a marker lands after its replacement.
    private func map(_ point: Int, through changes: [ComposerDraftChange]) -> Int {
        var shift = 0
        for change in changes {
            if point < change.range.lowerBound { break }
            if point < change.range.upperBound {
                return change.range.lowerBound + shift + change.length
            }
            shift += change.delta
        }
        return point + shift
    }
}

private let fence: Scalars = Array("```".unicodeScalars)

private func opensMarkdownFence(_ line: ArraySlice<Unicode.Scalar>) -> Bool {
    line.starts(with: fence)
}

private func closesMarkdownFence(_ line: ArraySlice<Unicode.Scalar>) -> Bool {
    let trimmed = line.drop(while: isKotlinWhitespace)
    return opensMarkdownFence(trimmed) && trimmed.dropFirst(fence.count).allSatisfy(isKotlinWhitespace)
}

/// Kotlin `CharSequence.trim()`: `Character.isWhitespace || isSpaceChar`.
private func isKotlinWhitespace(_ scalar: Unicode.Scalar) -> Bool {
    (0x09...0x0D).contains(scalar.value) || (0x1C...0x1F).contains(scalar.value) || isSeparator(scalar)
}

public struct ComposerDraftChange: Hashable, Sendable {
    public let range: ComposerScalarRange
    public let text: String

    init(_ range: ComposerScalarRange, _ text: String) {
        self.range = range
        self.text = text
    }

    var length: Int { text.unicodeScalars.count }
    var delta: Int { length - range.count }
}
