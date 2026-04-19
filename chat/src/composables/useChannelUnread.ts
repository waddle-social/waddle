import { computed, ref, type Ref } from "vue";
import type { BrowserXmppClient, RoomActivityEvent } from "@/lib/xmpp-client";
import { parseManagedRoomBareJid } from "@/lib/xmpp-client";

export interface ChannelUnreadEntry {
  roomJid: string;
  channelId: string;
  unreadCount: number;
  mentionCount: number;
}

export function useChannelUnread(
  xmppClient: Ref<BrowserXmppClient | null>,
) {
  const channelUnreads = ref<Map<string, ChannelUnreadEntry>>(new Map());
  let hydrated = false;
  let hydrateRequestId = 0;

  const totalUnreadCount = computed(() => {
    let total = 0;
    for (const entry of channelUnreads.value.values()) total += entry.unreadCount;
    return total;
  });

  const totalMentionCount = computed(() => {
    let total = 0;
    for (const entry of channelUnreads.value.values()) total += entry.mentionCount;
    return total;
  });

  async function hydrateFromInbox() {
    const client = xmppClient.value;
    if (!client) return;

    const requestId = ++hydrateRequestId;
    try {
      const inbox = await client.fetchInbox();
      if (requestId !== hydrateRequestId || client !== xmppClient.value) return;

      const next = new Map<string, ChannelUnreadEntry>();
      for (const entry of inbox.conversations) {
        if (entry.kind !== "muc") continue;
        const parsed = parseManagedRoomBareJid(entry.partner);
        if (!parsed) continue;
        next.set(entry.partner, {
          roomJid: entry.partner,
          channelId: parsed.channelId,
          unreadCount: entry.unread,
          mentionCount: 0,
        });
      }

      channelUnreads.value = next;
      hydrated = true;
    } catch {
      // best-effort
    }
  }

  function incrementUnread(event: RoomActivityEvent) {
    if (!hydrated) return;
    const roomJid = event.roomJid;
    const hasMention = !!(event.mentions?.length || event.broadcastMention);
    const existing = channelUnreads.value.get(roomJid);
    const next = new Map(channelUnreads.value);
    if (existing) {
      next.set(roomJid, {
        ...existing,
        unreadCount: existing.unreadCount + 1,
        mentionCount: existing.mentionCount + (hasMention ? 1 : 0),
      });
    } else {
      const parsed = parseManagedRoomBareJid(roomJid);
      if (!parsed) return;
      next.set(roomJid, {
        roomJid,
        channelId: parsed.channelId,
        unreadCount: 1,
        mentionCount: hasMention ? 1 : 0,
      });
    }
    channelUnreads.value = next;
  }

  function clearUnread(roomJid: string) {
    const existing = channelUnreads.value.get(roomJid);
    if (!existing || (existing.unreadCount === 0 && existing.mentionCount === 0)) return;
    const next = new Map(channelUnreads.value);
    next.set(roomJid, { ...existing, unreadCount: 0, mentionCount: 0 });
    channelUnreads.value = next;
  }

  function markRead(roomJid: string) {
    clearUnread(roomJid);
    const client = xmppClient.value;
    if (client) {
      void client.markInboxRead(roomJid).catch(() => {});
    }
  }

  function channelUnreadMap(): Record<string, { unread: number; mentions: number }> {
    const map: Record<string, { unread: number; mentions: number }> = {};
    for (const entry of channelUnreads.value.values()) {
      map[entry.channelId] = { unread: entry.unreadCount, mentions: entry.mentionCount };
    }
    return map;
  }

  return {
    channelUnreads,
    totalUnreadCount,
    totalMentionCount,
    hydrateFromInbox,
    incrementUnread,
    clearUnread,
    markRead,
    channelUnreadMap,
  };
}
