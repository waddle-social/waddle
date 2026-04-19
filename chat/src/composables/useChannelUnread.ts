import { computed, ref, type Ref } from "vue";
import type { BrowserXmppClient, InboxEntry } from "@/lib/xmpp-client";
import { parseManagedRoomBareJid } from "@/lib/xmpp-client";
import {
  createInboxState,
  applyEntry,
  applyEntries,
  markReadInState,
  threadsForRoom,
  type InboxState,
} from "@/services/inbox";

export interface ThreadInboxEntry {
  roomJid: string;
  threadId: string;
  title?: string;
  lastUpdated: number;
  unread: number;
  replyCount: number;
  author?: string;
  preview?: string;
}

export function useChannelUnread(
  xmppClient: Ref<BrowserXmppClient | null>,
) {
  const inboxState = ref<InboxState>(createInboxState());
  let hydrateRequestId = 0;

  const totalUnreadCount = computed(() => {
    let total = 0;
    for (const entry of inboxState.value.channels.values()) {
      if (entry.kind !== "muc") continue;
      total += entry.unread;
    }
    return total;
  });

  const totalMentionCount = computed(() => {
    // Server doesn't track mentions separately yet — return 0
    return 0;
  });

  async function hydrateFromInbox() {
    const client = xmppClient.value;
    if (!client) return;

    const requestId = ++hydrateRequestId;
    try {
      const inbox = await client.fetchInbox();
      if (requestId !== hydrateRequestId || client !== xmppClient.value) return;

      inboxState.value = applyEntries(createInboxState(), inbox.conversations);
    } catch {
      // best-effort
    }
  }

  /** Handle a server-pushed inbox entry (headline message with absolute unread count). */
  function onInboxPush(entry: InboxEntry) {
    inboxState.value = applyEntry(inboxState.value, entry);
  }

  function clearUnread(roomJid: string) {
    inboxState.value = markReadInState(inboxState.value, roomJid);
  }

  function markRead(roomJid: string) {
    clearUnread(roomJid);
    const client = xmppClient.value;
    if (client) {
      void client.markInboxRead(roomJid).catch(() => {});
    }
  }

  function markThreadRead(roomJid: string, threadId: string) {
    inboxState.value = markReadInState(inboxState.value, roomJid, threadId);
    const client = xmppClient.value;
    if (client) {
      void client.markInboxRead(roomJid, threadId).catch(() => {});
    }
  }

  function channelUnreadMap(): Record<string, { unread: number; mentions: number }> {
    const map: Record<string, { unread: number; mentions: number }> = {};
    for (const entry of inboxState.value.channels.values()) {
      if (entry.kind !== "muc") continue;
      const parsed = parseManagedRoomBareJid(entry.partner);
      if (!parsed) continue;
      map[parsed.channelId] = { unread: entry.unread, mentions: 0 };
    }
    return map;
  }

  function threadEntries(roomJid: string): ThreadInboxEntry[] {
    return threadsForRoom(inboxState.value, roomJid).map((entry) => ({
      roomJid: entry.partner,
      threadId: entry.thread!,
      title: entry.threadTitle ?? entry.preview,
      lastUpdated: entry.lastUpdated,
      unread: entry.unread,
      replyCount: entry.replyCount ?? 0,
      author: entry.author,
      preview: entry.preview,
    }));
  }

  return {
    inboxState,
    totalUnreadCount,
    totalMentionCount,
    hydrateFromInbox,
    onInboxPush,
    clearUnread,
    markRead,
    markThreadRead,
    channelUnreadMap,
    threadEntries,
  };
}
