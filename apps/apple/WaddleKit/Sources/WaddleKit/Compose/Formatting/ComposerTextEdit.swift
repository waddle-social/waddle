/// Half-open draft offsets in Unicode scalars, measured against the same draft
/// string. An empty range represents a caret; native editors convert UTF-16 at the boundary.
public typealias ComposerScalarRange = Range<Int>

/// The draft after a formatting or insertion edit, and where the caret or
/// selection lands.
public struct ComposerTextEdit: Hashable, Sendable {
    public let text: String
    public let selection: ComposerScalarRange
    let changes: [ComposerDraftChange]

    public init(text: String, selection: ComposerScalarRange) {
        self.init(text: text, selection: selection, changes: [])
    }

    init(text: String, selection: ComposerScalarRange, changes: [ComposerDraftChange]) {
        self.text = text
        self.selection = selection
        self.changes = changes
    }

    public var replacement: ComposerTextReplacement {
        changes.isEmpty ? .inferred(text) : .changes(text, changes)
    }

    /// Edits compare by the resulting draft and selection.
    public static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.text == rhs.text && lhs.selection == rhs.selection
    }

    public func hash(into hasher: inout Hasher) {
        hasher.combine(text)
        hasher.combine(selection)
    }
}

/// New draft text with the old scalar ranges available for reference relocation.
public enum ComposerTextReplacement: Sendable {
    case inferred(String)
    case range(String, ComposerScalarRange)
    case changes(String, [ComposerDraftChange])

    public init(_ text: String, replacing range: ComposerScalarRange? = nil) {
        self = range.map { .range(text, $0) } ?? .inferred(text)
    }

    public var text: String {
        switch self {
        case let .inferred(text), let .range(text, _), let .changes(text, _): return text
        }
    }

    /// Replacements in application order, measured against the original draft.
    public func nativeChanges(from original: String) -> [ComposerDraftChange] {
        if case let .changes(_, changes) = self { return Array(changes.reversed()) }

        let old = Array(original.unicodeScalars)
        let new = Array(text.unicodeScalars)
        let delta = new.count - old.count
        if case let .range(_, range) = self, range.lowerBound >= 0, range.upperBound <= old.count,
           range.count + delta >= 0,
           old[..<range.lowerBound].elementsEqual(new[..<range.lowerBound]),
           old[range.upperBound...].elementsEqual(new[(range.upperBound + delta)...]) {
            let inserted = new[range.lowerBound..<(range.upperBound + delta)]
            return range.isEmpty && inserted.isEmpty ? [] : [ComposerDraftChange(range, String(String.UnicodeScalarView(inserted)))]
        }

        let prefix = zip(old, new).prefix { $0 == $1 }.count
        if prefix == old.count, prefix == new.count { return [] }
        let suffix = zip(old.dropFirst(prefix).reversed(), new.dropFirst(prefix).reversed()).prefix { $0 == $1 }.count
        let inserted = new[prefix..<(new.count - suffix)]
        return [ComposerDraftChange(prefix..<(old.count - suffix), String(String.UnicodeScalarView(inserted)))]
    }

    /// Keeps the old range even when the insertion has an identical prefix or text.
    public static func replacingSelection(
        with insertion: String, in text: String, selection: ComposerScalarRange?
    ) -> (edit: ComposerTextEdit, replacement: Self) {
        let range = ComposerScalars.clamp(selection, count: text.unicodeScalars.count)
        let edit = ComposerInsertion.replacingSelection(with: insertion, in: text, selection: range)
        return (edit, Self(edit.text, replacing: range))
    }

    public static func applyingFormat(
        _ format: ComposerFormat, to text: String, selection: ComposerScalarRange?
    ) -> (edit: ComposerTextEdit, replacement: Self) {
        let edit = ComposerFormatting.apply(format, to: text, selection: selection)
        return (edit, edit.replacement)
    }
}
