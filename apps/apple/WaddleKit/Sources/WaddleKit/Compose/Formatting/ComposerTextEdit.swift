/// Half-open draft offsets in Unicode scalars, measured against the same draft
/// string. An empty range represents a caret; native editors convert UTF-16 at the boundary.
public typealias ComposerScalarRange = Range<Int>

/// The draft after a formatting or insertion edit, and where the caret or
/// selection lands.
public struct ComposerTextEdit: Hashable, Sendable {
    public let text: String
    public let selection: ComposerScalarRange

    public init(text: String, selection: ComposerScalarRange) {
        self.text = text
        self.selection = selection
    }
}

/// A change to the draft and, when known, the range it replaced in the old text.
public struct ComposerTextReplacement: Sendable {
    public let text: String
    public let range: ComposerScalarRange?
    /// Where the old range's text lands when formatting wraps it.
    let preservedRange: ComposerScalarRange?

    public init(_ text: String, replacing range: ComposerScalarRange? = nil) {
        self.text = text
        self.range = range
        self.preservedRange = nil
    }

    private init(_ text: String, wrapping range: ComposerScalarRange, at preservedRange: ComposerScalarRange) {
        self.text = text
        self.range = range
        self.preservedRange = preservedRange
    }

    /// Keeps the old range even when the insertion has an identical prefix or text.
    public static func replacingSelection(
        with insertion: String, in text: String, selection: ComposerScalarRange?
    ) -> (edit: ComposerTextEdit, replacement: Self) {
        let range = ComposerScalars.clamp(selection, count: text.unicodeScalars.count)
        let edit = ComposerInsertion.replacingSelection(with: insertion, in: text, selection: range)
        return (edit, Self(edit.text, replacing: range))
    }

    /// Keeps the wrapped text at its known destination, even when it contains the same markers.
    public static func applyingFormat(
        _ format: ComposerFormat, to text: String, selection: ComposerScalarRange?
    ) -> (edit: ComposerTextEdit, replacement: Self) {
        let edit = ComposerFormatting.apply(format, to: text, selection: selection)
        // Quotes prefix each line. The other formats insert markers at the selection's edges.
        guard format != .quote else { return (edit, Self(edit.text)) }
        let scalars = text.unicodeScalars
        let start = if let selection {
            ComposerScalars.clamp(selection, count: scalars.count).lowerBound
        } else {
            // Without a selection, the formatter wraps the draft after its leading whitespace.
            scalars.prefix(while: { $0.properties.isWhitespace }).count
        }
        let range = start..<(start + edit.selection.count)
        return (edit, Self(edit.text, wrapping: range, at: edit.selection))
    }
}
