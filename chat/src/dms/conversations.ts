import { computed, ref, watch, type Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient, DmConversation, InboxEntry, LiveDmMessage, PresenceUpdateEvent } from "@/lib/xmpp-client";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp-client";

function peerUsername(peerJid: string): string {
  return jidLocalpart(peerJid);
}

function conversationTimestamp(value?: string): number {
  if (!value) return 0;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

function inboxTimestamp(value: number): number {
  if (!Number.isFinite(value) || value <= 0) return 0;
  return value > 1_000_000_000_000 ? value : value * 1000;
}

function inboxTimestampIso(value: number): string | undefined {
  const timestamp = inboxTimestamp(value);
  return timestamp > 0 ? new Date(timestamp).toISOString() : undefined;
}

function sortByRecent(conversations: DmConversation[]): DmConversation[] {
  return [...conversations].sort((a, b) => {
    const at = conversationTimestamp(a.lastMessageAt);
    const bt = conversationTimestamp(b.lastMessageAt);
    return bt - at;
  });
}

export function useDirectMessageConversations(
  session: Ref<WaddleSession | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
) {
  const conversations = ref<DmConversation[]>([]);
  const activePeerJid = ref<string | null>(null);
  const presenceByJid = ref<Record<string, DmConversation["presenceShow"]>>({});
  // Per-peer XEP-0319 idle instant (epoch ms) behind an away dot; ephemeral
  // (repopulated by live presence), so unlike unread it is not persisted.
  const presenceIdleByJid = ref<Record<string, number | undefined>>({});
  const localReadAtByJid = ref<Record<string, number>>({});

  const hasUnread = computed(() => conversations.value.some((c) => c.unreadCount > 0));
  const totalUnreadCount = computed(() =>
    conversations.value.reduce((total, conversation) => total + Math.max(0, conversation.unreadCount), 0)
  );
  const selfBareJid = computed(() => barePeerJid(session.value?.jid ?? ""));

  let inboxRequestId = 0;
  const pendingMarkRead = new Set<string>();
  const queuedMarkRead = new Set<string>();
  const inboxAccountedMessageIdsByJid = new Map<string, Set<string>>();
  let lastPeerCallHydrationKey = "";
  let lastPeerCallHydrationClient: BrowserXmppClient | null = null;

  function storageKey() {
    const bare = session.value ? barePeerJid(session.value.jid) : "";
    return bare ? `waddle.chat.dms.${bare}` : "";
  }

  function persist() {
    if (typeof window === "undefined") return;
    const key = storageKey();
    if (!key) return;
    window.sessionStorage.setItem(
      key,
      JSON.stringify({
        // presenceIdleSince is ephemeral (repopulated by live presence). Strip
        // it before persisting — JSON.stringify drops the `undefined` key — so a
        // stale "idle 20m" can't survive a reload. presenceShow is still
        // persisted so the dot doesn't flash offline before presence arrives.
        conversations: conversations.value.map((c) => ({ ...c, presenceIdleSince: undefined })),
        activePeerJid: activePeerJid.value,
      }),
    );
  }

  function restore() {
    if (typeof window === "undefined") return;
    const key = storageKey();
    if (!key) return;
    try {
      const raw = window.sessionStorage.getItem(key);
      if (!raw) return;
      const parsed = JSON.parse(raw) as { conversations?: DmConversation[]; activePeerJid?: string | null };
      conversations.value = sortByRecent((parsed.conversations ?? []).map((c) => ({
        ...c,
        peerJid: barePeerJid(c.peerJid),
        peerUsername: c.peerUsername || peerUsername(c.peerJid),
        unreadCount: c.unreadCount ?? 0,
        // Clear any idle age from an older persisted blob — it starts empty and
        // only repopulates from live presence updates.
        presenceIdleSince: undefined,
      })));
      activePeerJid.value = parsed.activePeerJid ? barePeerJid(parsed.activePeerJid) : null;
      for (const c of conversations.value) {
        if (c.presenceShow) {
          presenceByJid.value[c.peerJid] = c.presenceShow;
        }
      }
    } catch {
      conversations.value = [];
      activePeerJid.value = null;
    }
  }

  function ensureConversation(peerJid: string): DmConversation {
    const bare = barePeerJid(peerJid);
    const existing = conversations.value.find((c) => c.peerJid === bare);
    if (existing) return existing;
    const created: DmConversation = {
      peerJid: bare,
      peerUsername: peerUsername(bare),
      unreadCount: 0,
      presenceShow: presenceByJid.value[bare] ?? "offline",
      presenceIdleSince: presenceIdleByJid.value[bare],
    };
    conversations.value = sortByRecent([...conversations.value, created]);
    return created;
  }

  function rememberRead(peerJid: string, lastMessageAt?: string) {
    const bare = barePeerJid(peerJid);
    const readAt = Math.max(
      Math.floor(Date.now() / 1000),
      Math.floor(conversationTimestamp(lastMessageAt) / 1000),
    );
    localReadAtByJid.value = { ...localReadAtByJid.value, [bare]: readAt };
  }

  function syncMarkRead(peerJid: string) {
    const bare = barePeerJid(peerJid);
    const client = xmppClient.value;
    if (!client) return;
    if (pendingMarkRead.has(bare)) {
      queuedMarkRead.add(bare);
      return;
    }
    pendingMarkRead.add(bare);
    void client.markInboxRead(bare)
      .catch(() => undefined)
      .finally(() => {
        pendingMarkRead.delete(bare);
        if (queuedMarkRead.delete(bare)) {
          syncMarkRead(bare);
        }
      });
  }

  function rememberInboxAccountedMessage(entry: InboxEntry) {
    if (!entry.lastStanzaId) return;
    const bare = barePeerJid(entry.partner);
    const messageIds = inboxAccountedMessageIdsByJid.get(bare) ?? new Set<string>();
    messageIds.add(entry.lastStanzaId);
    while (messageIds.size > 20) {
      const oldest = messageIds.values().next().value;
      if (!oldest) break;
      messageIds.delete(oldest);
    }
    inboxAccountedMessageIdsByJid.set(bare, messageIds);
  }

  function wasUnreadAccountedByInbox(peerJid: string, msg: LiveDmMessage): boolean {
    const messageIds = inboxAccountedMessageIdsByJid.get(barePeerJid(peerJid));
    if (!messageIds) return false;
    return [msg.id, ...(msg.wireIds ?? [])].some((messageId) => messageIds.has(messageId));
  }

  function mergeInboxEntry(existing: DmConversation | undefined, entry: InboxEntry): DmConversation {
    const bare = barePeerJid(entry.partner);
    const serverTimestamp = inboxTimestamp(entry.lastUpdated);
    const serverLastMessageAt = inboxTimestampIso(entry.lastUpdated);
    const existingTimestamp = conversationTimestamp(existing?.lastMessageAt);
    const useServerPreview = serverTimestamp >= existingTimestamp;
    const localReadAt = localReadAtByJid.value[bare] ?? 0;
    const lastMessageBody = useServerPreview
      ? entry.preview ?? existing?.lastMessageBody
      : existing?.lastMessageBody;
    const lastMessageAt = useServerPreview
      ? serverLastMessageAt ?? existing?.lastMessageAt
      : existing?.lastMessageAt;

    return {
      peerJid: bare,
      peerUsername: existing?.peerUsername || peerUsername(bare),
      ...(existing?.peerAvatarUrl !== undefined ? { peerAvatarUrl: existing.peerAvatarUrl } : {}),
      ...(lastMessageBody ? { lastMessageBody } : {}),
      ...(lastMessageAt ? { lastMessageAt } : {}),
      unreadCount: localReadAt >= entry.lastUpdated
        ? 0
        : existingTimestamp >= serverTimestamp
          ? Math.max(existing?.unreadCount ?? 0, entry.unread)
          : entry.unread,
      presenceShow: presenceByJid.value[bare] ?? existing?.presenceShow ?? "offline",
      presenceIdleSince: presenceIdleByJid.value[bare] ?? existing?.presenceIdleSince,
    };
  }

  function mergeInboxConversations(entries: InboxEntry[]) {
    const merged = new Map(conversations.value.map((conversation) => [conversation.peerJid, conversation]));

    for (const entry of entries) {
      if (entry.kind !== "direct") continue;
      const bare = barePeerJid(entry.partner);
      rememberInboxAccountedMessage(entry);
      merged.set(bare, mergeInboxEntry(merged.get(bare), entry));
    }

    conversations.value = sortByRecent([...merged.values()]);
  }

  function onInboxPush(entry: InboxEntry) {
    if (entry.kind !== "direct") return;
    mergeInboxConversations([entry]);
  }

  async function hydrateCurrentDmCallActivities(
    currentClient: BrowserXmppClient,
    currentSessionJid: string,
    requestId: number,
  ): Promise<void> {
    if (
      requestId !== inboxRequestId
      || currentClient !== xmppClient.value
      || currentSessionJid !== (session.value?.jid ?? null)
    ) return;
    await currentClient.hydrateRecentDmCallActivities().catch(() => undefined);
  }

  function hydratePeerDmCallActivity(peerJid: string) {
    const bare = barePeerJid(peerJid);
    const currentClient = xmppClient.value;
    const currentSessionJid = session.value?.jid ?? null;
    if (!bare || !currentClient || !currentSessionJid) return;
    const hydrateRecentDmCallActivity = currentClient.hydrateRecentDmCallActivity?.bind(currentClient);
    if (!hydrateRecentDmCallActivity) return;
    const hydrationKey = `${currentSessionJid}\n${bare}`;
    if (hydrationKey === lastPeerCallHydrationKey && currentClient === lastPeerCallHydrationClient) return;
    lastPeerCallHydrationKey = hydrationKey;
    lastPeerCallHydrationClient = currentClient;
    void hydrateRecentDmCallActivity(bare).catch(() => {
      if (lastPeerCallHydrationKey === hydrationKey && lastPeerCallHydrationClient === currentClient) {
        lastPeerCallHydrationKey = "";
        lastPeerCallHydrationClient = null;
      }
    });
  }

  async function hydrateFromInbox(): Promise<boolean> {
    const currentClient = xmppClient.value;
    const currentSessionJid = session.value?.jid ?? null;
    if (!currentClient || !currentSessionJid) return false;

    const requestId = ++inboxRequestId;
    const dmCallActivityHydration = hydrateCurrentDmCallActivities(
      currentClient,
      currentSessionJid,
      requestId,
    );
    try {
      const inbox = await currentClient.fetchInbox();
      // Superseded is success, not failure: a newer hydrate (or session)
      // owns the inbox now — reporting `false` would make retry chains
      // re-fetch and supersede each other in a loop.
      if (
        requestId !== inboxRequestId
        || currentClient !== xmppClient.value
        || currentSessionJid !== (session.value?.jid ?? null)
      ) return true;

      const directConversations = inbox.conversations.filter((conversation) => conversation.kind === "direct");
      mergeInboxConversations(directConversations);
      for (const conversation of directConversations) {
        void currentClient.subscribeToPeerPresence(barePeerJid(conversation.partner)).catch(() => undefined);
      }
      void dmCallActivityHydration;
      return true;
    } catch {
      void dmCallActivityHydration;
      // best-effort
      return false;
    }
  }

  function markRead(peerJid: string, opts: { forceSync?: boolean } = {}) {
    const bare = barePeerJid(peerJid);
    const conversation = ensureConversation(bare);
    rememberRead(bare, conversation.lastMessageAt);

    let shouldSync = !!opts.forceSync;
    conversations.value = conversations.value.map((c) => {
      if (c.peerJid !== bare) return c;
      shouldSync = shouldSync || c.unreadCount > 0;
      return c.unreadCount > 0 ? { ...c, unreadCount: 0 } : c;
    });

    if (shouldSync) {
      syncMarkRead(bare);
    }
  }

  async function openDm(peerJid: string) {
    const bare = barePeerJid(peerJid);
    ensureConversation(bare);
    activePeerJid.value = bare;
    hydratePeerDmCallActivity(bare);
    try {
      await xmppClient.value?.subscribeToPeerPresence(bare);
    } catch {
      // best-effort
    }
  }

  function closeDm() {
    activePeerJid.value = null;
  }

  function receiveIncomingDm(msg: LiveDmMessage) {
    const bare = barePeerJid(msg.peerJid);
    const existing = ensureConversation(bare);
    const isSelfMessage = barePeerJid(msg.fromJid) === selfBareJid.value;
    const isActiveConversation = activePeerJid.value === bare;
    // Archive-decoded arrivals are MAM catch-up re-emissions: the message
    // may already have been counted live before a reconnect, and
    // genuinely-missed messages are accounted by the server inbox
    // (hydrateFromInbox runs on every session-ready). Only live arrivals
    // increment locally.
    const shouldIncrementUnread = !isSelfMessage
      && !isActiveConversation
      && msg.createdAtSource !== "archive"
      && !wasUnreadAccountedByInbox(bare, msg);

    conversations.value = sortByRecent(conversations.value.map((c) => {
      if (c.peerJid !== bare) return c;
      // Monotonic preview: an archive re-emission of an OLDER message
      // (MAM catch-up after a reconnect) must not roll the preview or the
      // recency ordering back behind a newer live arrival.
      const isNewestMessage =
        conversationTimestamp(msg.createdAt) >= conversationTimestamp(c.lastMessageAt);
      return {
        ...c,
        peerUsername: existing.peerUsername || peerUsername(bare),
        ...(isNewestMessage ? { lastMessageBody: msg.body, lastMessageAt: msg.createdAt } : {}),
        unreadCount: shouldIncrementUnread ? c.unreadCount + 1 : c.unreadCount,
        presenceShow: presenceByJid.value[bare] ?? c.presenceShow,
        presenceIdleSince: presenceIdleByJid.value[bare] ?? c.presenceIdleSince,
      };
    }));
  }

  function updatePresence(event: PresenceUpdateEvent) {
    const bare = barePeerJid(event.bareJid);
    presenceByJid.value = { ...presenceByJid.value, [bare]: event.show };
    // Always overwrite (including to undefined) so returning to Available
    // clears a previously rendered idle age rather than stranding it.
    presenceIdleByJid.value = { ...presenceIdleByJid.value, [bare]: event.idleSince };
    conversations.value = conversations.value.map((c) => (
      c.peerJid === bare
        ? { ...c, presenceShow: event.show, presenceIdleSince: event.idleSince }
        : c
    ));
  }

  watch(
    () => session.value?.jid,
    () => {
      inboxRequestId += 1;
      lastPeerCallHydrationKey = "";
      lastPeerCallHydrationClient = null;
      pendingMarkRead.clear();
      queuedMarkRead.clear();
      inboxAccountedMessageIdsByJid.clear();
      conversations.value = [];
      activePeerJid.value = null;
      presenceByJid.value = {};
      presenceIdleByJid.value = {};
      localReadAtByJid.value = {};
      restore();
      for (const c of conversations.value) {
        xmppClient.value?.subscribeToPeerPresence(c.peerJid);
      }
      if (activePeerJid.value) hydratePeerDmCallActivity(activePeerJid.value);
    },
    { immediate: true },
  );

  watch(
    [xmppClient, activePeerJid],
    () => {
      if (activePeerJid.value) hydratePeerDmCallActivity(activePeerJid.value);
    },
  );

  watch([conversations, activePeerJid], persist, { deep: true });

  return {
    conversations,
    activePeerJid,
    hasUnread,
    totalUnreadCount,
    openDm,
    closeDm,
    hydrateFromInbox,
    onInboxPush,
    markRead,
    receiveIncomingDm,
    updatePresence,
  };
}
