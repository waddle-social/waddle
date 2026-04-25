/**
 * Private Waddle inbox.
 *
 * Mirrors the Rust types in `server/crates/waddle-xmpp/src/inbox/mod.rs`
 * and the protocol wrapper in `server/crates/waddle-xmpp/src/xep/xep0430.rs`.
 *
 * Wire format:
 *
 * Request:
 *   <iq type='get'>
 *     <query xmlns='urn:waddle:inbox:0' since='1700000' only-unread='true'/>
 *   </iq>
 *
 * Result:
 *   <iq type='result'>
 *     <query xmlns='urn:waddle:inbox:0' total-unread='3'>
 *       <conversation partner='alice@example.com' kind='direct'
 *                     last-stanza-id='sid' last-updated='1700000' unread='2'>
 *         <preview>hi there</preview>
 *       </conversation>
 *     </query>
 *   </iq>
 *
 * Mark read:
 *   <iq type='set'>
 *     <mark-read xmlns='urn:waddle:inbox:0' partner='alice@example.com'/>
 *   </iq>
 */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute, booleanAttribute, childText, integerAttribute } from "stanza/jxt";

export const NS_WADDLE_INBOX_0 = "urn:waddle:inbox:0";

export type InboxConversationKind = "direct" | "muc";

export interface WaddleInboxConversation {
  partner: string;
  kind: InboxConversationKind;
  lastStanzaId: string;
  lastUpdated: number;
  unread: number;
  preview?: string;
  thread?: string;
  threadTitle?: string;
  replyCount?: number;
  author?: string;
}

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "iq.inbox", multiple: false }],
    element: "query",
    fields: {
      since: integerAttribute("since"),
      onlyUnread: booleanAttribute("only-unread"),
      room: attribute("room"),
      threads: booleanAttribute("threads"),
      totalUnread: integerAttribute("total-unread"),
    },
    namespace: NS_WADDLE_INBOX_0,
  },
  {
    aliases: [
      { path: "iq.inbox.conversations", multiple: true },
      { path: "message.inboxPush", multiple: false },
    ],
    element: "conversation",
    fields: {
      partner: attribute("partner"),
      kind: attribute("kind"),
      lastStanzaId: attribute("last-stanza-id"),
      lastUpdated: integerAttribute("last-updated"),
      unread: integerAttribute("unread"),
      preview: childText(NS_WADDLE_INBOX_0, "preview"),
      thread: attribute("thread"),
      threadTitle: attribute("thread-title"),
      replyCount: integerAttribute("reply-count"),
      author: attribute("author"),
    },
    namespace: NS_WADDLE_INBOX_0,
  },
  {
    aliases: [{ path: "iq.inboxMarkRead", multiple: false }],
    element: "mark-read",
    fields: {
      partner: attribute("partner"),
      thread: attribute("thread"),
    },
    namespace: NS_WADDLE_INBOX_0,
  },
];

export default definitions;
