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
