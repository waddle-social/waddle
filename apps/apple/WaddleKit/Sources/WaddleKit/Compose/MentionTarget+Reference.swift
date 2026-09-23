import Foundation

extension MentionTarget {
    /// The target a XEP-0372 mention names, reversing `uri`. Nil for other
    /// reference kinds and for URIs the composer never produces, such as a
    /// full (occupant) JID, which must not widen to its bare room JID.
    public init?(reference: Reference) {
        guard reference.kind == .mention else { return nil }
        switch reference.uri {
        case MentionTarget.everyone.uri: self = .everyone
        case MentionTarget.here.uri: self = .here
        default:
            guard let jid = reference.mentionedJID, jid.resource == nil else { return nil }
            self = .user(jid.bare)
        }
    }
}
