import Foundation
import WaddleKit

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

    /// The layout input for a row. `hiddenPrefix` scalars at the start of
    /// the body are not rendered (a XEP-0245 action line's "/me ").
    static func input(for item: TimelineItem, account: AccountIdentity, hiddenPrefix: Int = 0) -> RichTextInput {
        RichTextInput(
            displayedBody: item.body,
            wireBody: item.message.body,
            fallback: item.message.reply?.fallback,
            isEdited: item.isEdited,
            spans: item.message.markupSpans,
            references: item.message.references,
            broadcastSpans: item.message.broadcastSpans,
            ownNick: account.nick,
            mentionKind: { kind(of: $0, account: account, in: item.conversation) },
            hiddenPrefix: hiddenPrefix
        )
    }
}
