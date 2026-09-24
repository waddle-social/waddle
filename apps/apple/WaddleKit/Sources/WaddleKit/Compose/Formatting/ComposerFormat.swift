import Foundation

/// A formatting-bar style. Each one inserts the literal markdown that
/// `ComposerMarkdown` turns into a XEP-0394 span at send time; there is no
/// link style because `ComposerMarkdown` has no link markup (receivers
/// auto-link bare URLs instead).
public enum ComposerFormat: CaseIterable, Hashable, Sendable {
    case bold
    case italic
    case strikethrough
    case code
    case codeBlock
    case quote
}
