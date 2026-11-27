import Foundation

/// Decides how a XEP-0372 reference is highlighted for the signed-in
/// account: the same rule the coordinator uses for mention alerts.
/// Room-wide `@everyone` / `@here` are XEP-0513 spans, not references.
enum MentionHighlight {
    static func kind(of reference: Reference, account: AccountIdentity, in conversation: ConversationID) -> RichMentionKind? {
        guard reference.kind == .mention else { return nil }
        guard let jid = reference.mentionedJID else { return .someone }
        if jid.bare == account.jid {
            return .me
        }
        if conversation.isRoom, jid.bare == conversation.jid, jid.resource == account.nick {
            return .me
        }
        return .someone
    }
}
