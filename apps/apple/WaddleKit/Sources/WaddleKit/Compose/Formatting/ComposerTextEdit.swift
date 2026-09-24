import Foundation

/// The draft after a formatting or insertion edit, and where the caret or
/// selection lands. Offsets are Unicode scalars, end exclusive.
public struct ComposerTextEdit: Hashable, Sendable {
    public let text: String
    public let selection: Range<Int>

    public init(text: String, selection: Range<Int>) {
        self.text = text
        self.selection = selection
    }
}
