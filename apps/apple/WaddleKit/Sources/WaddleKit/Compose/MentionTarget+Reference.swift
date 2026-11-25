import Foundation

extension MentionTarget {
    /// The user a XEP-0372 mention names. Nil for other reference kinds and
    /// for URIs the composer never produces, such as a full (occupant) JID,
    /// which must not widen to its bare room JID. Room-wide mentions are not
    /// references, so they never come back from here.
    public init?(reference: Reference) {
        guard reference.kind == .mention,
              let jid = reference.mentionedJID, jid.resource == nil
        else { return nil }
        self = .user(jid.bare)
    }
}
