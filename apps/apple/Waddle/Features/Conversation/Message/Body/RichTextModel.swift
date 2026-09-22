import Foundation

/// How strongly a XEP-0372 mention is highlighted.
enum RichMentionKind: Hashable, Sendable {
    case someone
    /// Addresses the signed-in account, directly or by broadcast.
    case me
}

/// An inline style over a run of text.
enum RichInlineStyle: Hashable, Sendable {
    case bold
    case italic
    case strikethrough
    case code
    case link(URL)
    case mention(RichMentionKind)
}

/// A run of text with one constant set of styles.
struct RichSegment: Hashable, Sendable {
    let text: String
    let styles: Set<RichInlineStyle>
}

/// A block of the rendered body.
enum RichBlock: Hashable, Sendable {
    case paragraph([RichSegment])
    /// A XEP-0394 blockquote with its `>` markers removed.
    case quote([RichSegment])
    /// A XEP-0394 code block, verbatim.
    case code(String)
}

/// A styled scalar range over a block's text.
struct RichStyledRange: Hashable, Sendable {
    let style: RichInlineStyle
    let range: Range<Int>
}

/// A link found by a detector, in Unicode scalar offsets over the text it
/// was given.
struct RichDetectedLink: Hashable, Sendable {
    let range: Range<Int>
    let url: URL
}
