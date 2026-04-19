/** XEP-0430 Inbox: high-level client helpers. */
import type { Agent } from "stanza";
import type { InboxConversationKind, WaddleInboxConversation } from "./extensions/inbox";

export type { InboxConversationKind } from "./extensions/inbox";

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
  /** When set with `threads: true`, return thread-level entries for this room. */
  room?: string;
  /** When true (requires `room`), return thread entries instead of channel entries. */
  threads?: boolean;
}

interface InboxIqResponse {
  inbox?: {
    totalUnread?: number;
    conversations?: WaddleInboxConversation[];
  };
}

function normaliseKind(raw: unknown): InboxConversationKind {
  return raw === "muc" ? "muc" : "direct";
}

function toEntry(raw: WaddleInboxConversation): InboxEntry {
  return {
    partner: raw.partner,
    kind: normaliseKind(raw.kind),
    lastStanzaId: raw.lastStanzaId,
    lastUpdated: typeof raw.lastUpdated === "number" ? raw.lastUpdated : Number(raw.lastUpdated ?? 0),
    unread: typeof raw.unread === "number" ? raw.unread : Number(raw.unread ?? 0),
    preview: raw.preview && raw.preview.length > 0 ? raw.preview : undefined,
    thread: raw.thread && raw.thread.length > 0 ? raw.thread : undefined,
    threadTitle: raw.threadTitle && raw.threadTitle.length > 0 ? raw.threadTitle : undefined,
    replyCount: typeof raw.replyCount === "number" ? raw.replyCount : undefined,
    author: raw.author && raw.author.length > 0 ? raw.author : undefined,
  };
}

export async function fetchInbox(xmpp: Agent, opts: FetchInboxOptions = {}): Promise<InboxResult> {
  const res = (await xmpp.sendIQ({
    type: "get",
    inbox: {
      since: opts.since,
      onlyUnread: opts.onlyUnread,
      room: opts.room,
      threads: opts.threads,
    },
  } as Parameters<Agent["sendIQ"]>[0])) as InboxIqResponse;
  const inbox = res?.inbox ?? {};
  const conversations = Array.isArray(inbox.conversations) ? inbox.conversations.map(toEntry) : [];
  return {
    totalUnread: typeof inbox.totalUnread === "number" ? inbox.totalUnread : 0,
    conversations,
  };
}

export async function markInboxRead(xmpp: Agent, partnerJid: string, threadId?: string): Promise<void> {
  await xmpp.sendIQ({
    type: "set",
    inboxMarkRead: {
      partner: partnerJid,
      ...(threadId ? { thread: threadId } : {}),
    },
  } as Parameters<Agent["sendIQ"]>[0]);
}
