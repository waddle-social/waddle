export type InboxConversationKind = "direct" | "muc";

export interface InboxEntry {
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

export interface InboxResult {
  /** XEP-0430 `<fin all-unread/>` — total unread across the whole inbox. */
  totalUnread: number;
  /** XEP-0430 `<fin total/>` — number of conversations in the page. */
  total: number;
  /** XEP-0430 `<fin unread/>` — number of conversations with unread > 0. */
  unreadConversations: number;
  conversations: InboxEntry[];
}

/**
 * XEP-0430 query options. The server defaults to `unread-only='false'`
 * + `messages='true'`, so the JS layer only needs to flip the bits
 * when it wants to deviate; the legacy Waddle-private `since`/`room`/
 * `threads` knobs are gone (room-thread paging lives behind
 * `fetchThreads` now).
 */
export interface FetchInboxOptions {
  onlyUnread?: boolean;
  /** When `true`, the server elides the embedded MAM `<result/>` body. */
  noMessages?: boolean;
}
