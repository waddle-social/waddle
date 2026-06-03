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

export interface ChannelThreadInboxEntry {
  roomJid: string;
  threadId: string;
  title?: string;
  lastUpdated: number;
  unread: number;
  replyCount: number;
  author?: string;
  preview?: string;
}

interface ChannelUnreadSummary {
  unread: number;
  mentions: number;
  threadUnread: number;
  preview?: string;
  lastUpdated?: number;
}

interface ReadClearBarrier {
  roomJid: string;
  threadId?: string;
  lastStanzaId: string;
}

export function useChannelInbox(
  xmppClient: Ref<BrowserXmppClient | null>,
) {
  const inboxState = ref<InboxState>(createInboxState());
  let hydrateRequestId = 0;
  let localMutationRevision = 0;
  let localMutations: Array<{
    revision: number;
    apply: (state: InboxState) => InboxState;
  }> = [];
  const readClearBarriers = new Map<string, ReadClearBarrier>();

  function inboxKey(roomJid: string, threadId?: string): string {
    return threadId ? `${roomJid}::${threadId}` : roomJid;
  }

  function barrierKey(roomJid: string, threadId?: string): string {
    return JSON.stringify([roomJid, threadId ?? null]);
  }

  function entryForKey(state: InboxState, roomJid: string, threadId?: string): InboxEntry | undefined {
    return threadId
      ? state.threads.get(inboxKey(roomJid, threadId))
      : state.channels.get(roomJid);
  }

  function currentEntryForIncoming(state: InboxState, entry: InboxEntry): InboxEntry | undefined {
    return entryForKey(state, entry.partner, entry.thread);
  }

  function applyFreshEntry(state: InboxState, entry: InboxEntry): InboxState {
    const existing = currentEntryForIncoming(state, entry);
    if (existing && entry.lastUpdated < existing.lastUpdated) return state;
    if (
      existing
      && entry.lastUpdated === existing.lastUpdated
      && entry.lastStanzaId !== existing.lastStanzaId
      && entry.unread <= existing.unread
    ) {
      return state;
    }
    return applyEntry(state, entry);
  }

  function applyReadClearBarrier(state: InboxState, barrier: ReadClearBarrier): InboxState {
    const entry = entryForKey(state, barrier.roomJid, barrier.threadId);
    if (!entry) return state;
    if (entry.lastStanzaId !== barrier.lastStanzaId) {
      readClearBarriers.delete(barrierKey(barrier.roomJid, barrier.threadId));
      return state;
    }

    return markReadInState(state, barrier.roomJid, barrier.threadId);
  }

  function applyReadClearBarrierFor(state: InboxState, roomJid: string, threadId?: string): InboxState {
    const barrier = readClearBarriers.get(barrierKey(roomJid, threadId));
    return barrier ? applyReadClearBarrier(state, barrier) : state;
  }

  function applyReadClearBarriers(state: InboxState): InboxState {
    let next = state;
    for (const barrier of readClearBarriers.values()) {
      next = applyReadClearBarrier(next, barrier);
    }
    return next;
  }

  function rememberReadClear(roomJid: string, threadId?: string) {
    const entry = entryForKey(inboxState.value, roomJid, threadId);
    if (entry) {
      readClearBarriers.set(barrierKey(roomJid, threadId), {
        roomJid,
        ...(threadId ? { threadId } : {}),
        lastStanzaId: entry.lastStanzaId,
      });
    }
  }

  function forgetReadClear(roomJid: string, threadId?: string) {
    readClearBarriers.delete(barrierKey(roomJid, threadId));
  }

  function applyLocalMutation(apply: (state: InboxState) => InboxState) {
    const revision = ++localMutationRevision;
    localMutations.push({ revision, apply });
    inboxState.value = apply(inboxState.value);
  }

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
      hydrateRequestId += 1;
      readClearBarriers.clear();
      applyLocalMutation(() => createInboxState());
      return false;
    }

    const requestId = ++hydrateRequestId;
    const startRevision = localMutationRevision;
    try {
      const inbox = await client.fetchInbox();
      if (requestId !== hydrateRequestId || client !== xmppClient.value) return false;

      let next = applyEntries(createInboxState(), inbox.conversations);
      next = applyReadClearBarriers(next);
      for (const mutation of localMutations) {
        if (mutation.revision > startRevision) {
          next = mutation.apply(next);
        }
      }
      inboxState.value = next;
      localMutations = [];
      return true;
    } catch {
      // best-effort
      return false;
    }
  }

  /** Handle a server-pushed inbox entry (headline message with absolute unread count). */
  function onInboxPush(entry: InboxEntry) {
    if (entry.kind !== "muc") return;
    applyLocalMutation((state) =>
      applyReadClearBarrierFor(applyFreshEntry(state, entry), entry.partner, entry.thread),
    );
  }

  function clearUnread(roomJid: string) {
    rememberReadClear(roomJid);
    applyLocalMutation((state) => applyReadClearBarrierFor(state, roomJid));
  }

  function clearAll() {
    hydrateRequestId += 1;
    readClearBarriers.clear();
    applyLocalMutation(() => createInboxState());
  }

  function markRead(roomJid: string) {
    clearUnread(roomJid);
    const client = xmppClient.value;
    if (client) {
      void client.markInboxRead(roomJid).catch(() => {
        forgetReadClear(roomJid);
        void hydrateFromInbox();
      });
    }
  }

  function markThreadRead(roomJid: string, threadId: string) {
    rememberReadClear(roomJid, threadId);
    applyLocalMutation((state) => applyReadClearBarrierFor(state, roomJid, threadId));
    const client = xmppClient.value;
    if (client) {
      void client.markInboxRead(roomJid, threadId).catch(() => {
        forgetReadClear(roomJid, threadId);
        void hydrateFromInbox();
      });
    }
  }

  /**
   * Build a `channelId → unread` map by matching topology channel JIDs
   * against inbox entries. Keying by JID (not partner-localpart) is
   * required: stale inbox rows from a different deployment can share a
   * localpart with a live room (e.g. `chat@muc.legacy.example` vs
   * `chat@muc.waddle.social`) and a localpart-keyed map silently
   * overwrites the live row, leaving its unread count stuck.
   */
  function channelUnreadMap(
    channels: readonly Pick<ChannelSummary, "id" | "jid">[],
  ): Record<string, ChannelUnreadSummary> {
    const map: Record<string, ChannelUnreadSummary> = {};
    for (const channel of channels) {
      if (!channel.jid) continue;
      const roomJid = barePeerJid(channel.jid);
      const entry = inboxState.value.channels.get(roomJid);
      const threadEntriesForRoom = threadsForRoom(inboxState.value, roomJid);
      const newest = [entry, ...threadEntriesForRoom]
        .filter((item): item is InboxEntry => !!item)
        .sort((a, b) => b.lastUpdated - a.lastUpdated)[0];
      map[channel.id] = {
        unread: entry?.unread ?? 0,
        mentions: 0,
        threadUnread: threadEntriesForRoom.reduce((total, thread) => total + thread.unread, 0),
        ...(newest?.preview || newest?.threadTitle ? { preview: newest.preview ?? newest.threadTitle } : {}),
        ...(newest?.lastUpdated ? { lastUpdated: newest.lastUpdated } : {}),
      };
    }
    return map;
  }

  function unreadForRoomJid(roomJid: string): number {
    return inboxState.value.channels.get(roomJid)?.unread ?? 0;
  }

  function threadEntries(roomJid: string): ChannelThreadInboxEntry[] {
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
