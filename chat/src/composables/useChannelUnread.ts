import { computed, ref, type Ref } from "vue";
import type { BrowserXmppClient, InboxEntry } from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp-client";
import type { ChannelSummary } from "@/lib/chat-types";
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

  const totalChannelUnreadCount = computed(() => {
    let total = 0;
    for (const entry of inboxState.value.channels.values()) {
      if (entry.kind !== "muc") continue;
      total += entry.unread;
    }
    return total;
  });

  const totalThreadUnreadCount = computed(() => {
    let total = 0;
    for (const entry of inboxState.value.threads.values()) {
      if (entry.kind !== "muc") continue;
      total += entry.unread;
    }
    return total;
  });

  // Server total unread counts conversation rows only; thread rows are nested detail.
  const totalUnreadCount = totalChannelUnreadCount;

  const totalMentionCount = computed(() => {
    // Server doesn't track mentions separately yet — return 0
    return 0;
  });

  async function hydrateFromInbox(): Promise<boolean> {
    const client = xmppClient.value;
    if (!client) {
      inboxState.value = createInboxState();
      return false;
    }

    const requestId = ++hydrateRequestId;
    try {
      const inbox = await client.fetchInbox();
      if (requestId !== hydrateRequestId || client !== xmppClient.value) return false;

      inboxState.value = applyEntries(createInboxState(), inbox.conversations);
      return true;
    } catch {
      // best-effort
      return false;
    }
  }

  /** Handle a server-pushed inbox entry (headline message with absolute unread count). */
  function onInboxPush(entry: InboxEntry) {
    if (entry.kind !== "muc") return;
    inboxState.value = applyEntry(inboxState.value, entry);
  }

  function clearUnread(roomJid: string) {
    inboxState.value = markReadInState(inboxState.value, roomJid);
  }

  function clearAll() {
    hydrateRequestId += 1;
    inboxState.value = createInboxState();
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

  /**
   * Build a `channelId → unread` map by matching topology channel JIDs
   * against inbox entries. Keying by JID (not partner-localpart) is
   * required: stale inbox rows from a different deployment can share a
   * localpart with a live room (e.g. `chat@muc.waddle.local` vs
   * `chat@muc.waddle.social`) and a localpart-keyed map silently
   * overwrites the live row, leaving its unread count stuck.
   */
  function channelUnreadMap(
    channels: readonly Pick<ChannelSummary, "id" | "jid">[],
  ): Record<string, { unread: number; mentions: number }> {
    const map: Record<string, { unread: number; mentions: number }> = {};
    for (const channel of channels) {
      if (!channel.jid) continue;
      const entry = inboxState.value.channels.get(barePeerJid(channel.jid));
      map[channel.id] = { unread: entry?.unread ?? 0, mentions: 0 };
    }
    return map;
  }

  function unreadForRoomJid(roomJid: string): number {
    return inboxState.value.channels.get(roomJid)?.unread ?? 0;
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
    totalChannelUnreadCount,
    totalThreadUnreadCount,
    totalUnreadCount,
    totalMentionCount,
    hydrateFromInbox,
    onInboxPush,
    clearUnread,
    clearAll,
    markRead,
    markThreadRead,
    channelUnreadMap,
    unreadForRoomJid,
    threadEntries,
  };
}
