import Foundation
import WaddleKit

/// Pure FFI → WaddleKit conversions. Every untyped FFI string (JID, URL,
/// timestamp) is parsed exactly once here; an optional value that fails
/// to parse becomes nil, and a record whose required value fails is
/// dropped.
enum FFIInbound {
    static func jid(_ raw: String?) -> JID? {
        raw.flatMap(JID.init(parsing:))
    }

    /// Strict: a full JID is rejected, not truncated.
    static func bareJID(_ raw: String?) -> BareJID? {
        raw.flatMap(BareJID.init(parsing:))
    }

    /// Absolute URLs only; a relative reference has no meaning here.
    static func url(_ raw: String?) -> URL? {
        guard let raw, let url = URL(string: raw), url.scheme != nil else { return nil }
        return url
    }

    static func date(_ raw: String?) -> Date? {
        raw.flatMap(FFIRFC3339.date(from:))
    }

    /// XEP-0428 fallback range from its two optional bounds; empty or
    /// inverted ranges are dropped.
    static func range(start: UInt32?, end: UInt32?) -> Range<Int>? {
        guard let start, let end, end > start else { return nil }
        return Int(start)..<Int(end)
    }
}
