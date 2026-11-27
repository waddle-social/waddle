import Foundation

/// How strongly a XEP-0372 mention is highlighted.
public enum RichMentionKind: Hashable, Sendable {
    case someone
    /// Addresses the signed-in account, directly or by broadcast.
    case me
}

/// An inline style over a run of text.
public enum RichInlineStyle: Hashable, Sendable {
    case bold
    case italic
    case strikethrough
    case code
    case link(URL)
    case roomLink(BareJID)
    case mention(RichMentionKind)

    var isLink: Bool {
        switch self {
        case .link, .roomLink: true
        default: false
        }
    }
}

/// A run of text with one constant set of styles.
public struct RichSegment: Hashable, Sendable {
    public let text: String
    public let styles: Set<RichInlineStyle>

    public init(text: String, styles: Set<RichInlineStyle>) {
        self.text = text
        self.styles = styles
    }
}

/// A block of the rendered body.
public enum RichBlock: Hashable, Sendable {
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
public struct RichDetectedLink: Hashable, Sendable {
    let range: Range<Int>
    let url: URL

    public init(range: Range<Int>, url: URL) {
        self.range = range
        self.url = url
    }
}
