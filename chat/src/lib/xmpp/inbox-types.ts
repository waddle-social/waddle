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
  totalUnread: number;
  conversations: InboxEntry[];
}

export interface FetchInboxOptions {
  since?: number;
  onlyUnread?: boolean;
  room?: string;
  threads?: boolean;
}
