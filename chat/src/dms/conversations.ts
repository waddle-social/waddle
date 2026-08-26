import { computed, ref, watch, type Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type {
  BrowserXmppClient,
  DmConversation,
  DmConversationScope,
  InboxEntry,
  LiveDmMessage,
  PresenceUpdateEvent,
} from "@/lib/xmpp-client";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp-client";

function peerUsername(peerJid: string): string {
  return jidLocalpart(peerJid);
}

/**
 * Display name for a MUC-PM conversation: "nick (room)" — the room
 * provenance keeps an occupant's self-chosen nick from rendering
 * byte-identical to a real account's DM entry (#1256 hardening). The
 * nick is the full resource (XEP-0045 nicks may contain '/').
 */
function mucPmDisplayName(occupantJid: string): string {
  const slash = occupantJid.indexOf("/");
  const nick = slash >= 0 ? occupantJid.slice(slash + 1) : occupantJid;
  return `${nick} (${jidLocalpart(occupantJid)})`;
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
  const activeConversationScope = computed<DmConversationScope | null>(() => {
    const active = activePeerJid.value;
    if (!active) return null;
    const conversation = conversations.value.find((candidate) => candidate.peerJid === active);
    return conversation?.mucPm ? "muc-occupant" : "account";
  });

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
      conversations.value = sortByRecent((parsed.conversations ?? []).map((c) => {
        const mucPm = c.mucPm === true || c.peerJid.includes("/");
        return {
          ...c,
          // #1256: MUC-PM conversations are keyed by the FULL occupant JID —
          // bare-folding on restore would re-key them to the room bare JID
          // (a reply from that row would broadcast to the room). The
          // resource heuristic backstops the flag: normal DM rows are
          // always persisted bare, so a resource-carrying key IS an
          // occupant conversation even in a blob written without the flag.
          peerJid: mucPm ? c.peerJid : barePeerJid(c.peerJid),
          peerUsername: c.peerUsername || (mucPm ? mucPmDisplayName(c.peerJid) : peerUsername(c.peerJid)),
          ...(mucPm ? { mucPm: true, mucPmRoomJid: c.mucPmRoomJid ?? barePeerJid(c.peerJid) } : {}),
          unreadCount: c.unreadCount ?? 0,
          // Clear any idle age from an older persisted blob — it starts empty and
          // only repopulates from live presence updates.
          presenceIdleSince: undefined,
        };
      }));
      const restoredActive = parsed.activePeerJid ?? null;
      activePeerJid.value = restoredActive
        ? (conversations.value.some((c) => c.peerJid === restoredActive.trim())
          ? restoredActive.trim()
          : barePeerJid(restoredActive))
        : null;
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

  /**
   * Canonical store key for a peer: MUC-PM conversations (#1256) are
   * keyed by the FULL occupant JID (`room@service/nick`). An exact match
   * on an existing conversation wins; otherwise a resource-carrying JID
   * whose bare part is a known MUC room stays occupant-keyed (so
   * initiating a PM never folds to the room bare JID, where a reply
   * would broadcast); everything else folds to the bare JID.
   */
  function conversationKeyFor(peerJid: string): string {
    const trimmed = peerJid.trim();
    if (conversations.value.some((c) => c.peerJid === trimmed)) return trimmed;
    if (xmppClient.value?.isMucPmPeer?.(trimmed)) return trimmed;
    return barePeerJid(trimmed);
  }

  function ensureConversation(key: string, username?: string, mucPmRoomJid?: string): DmConversation {
    const existing = conversations.value.find((c) => c.peerJid === key);
    if (existing) return existing;
    const created: DmConversation = {
      peerJid: key,
      peerUsername: username ?? peerUsername(key),
      ...(mucPmRoomJid ? { mucPm: true, mucPmRoomJid } : {}),
      unreadCount: 0,
      presenceShow: presenceByJid.value[key] ?? "offline",
      presenceIdleSince: presenceIdleByJid.value[key],
    };
    conversations.value = sortByRecent([...conversations.value, created]);
    return created;
  }

  function forgetPeer(peerJid: string) {
    const key = conversationKeyFor(peerJid);
    const next = conversations.value.filter((conversation) => conversation.peerJid !== key);
    if (next.length === conversations.value.length) return;
    conversations.value = next;
    if (activePeerJid.value === key) activePeerJid.value = null;
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
      // #1256: the server's inbox projects MUC PMs under the room bare
      // JID (it can't attribute the occupant yet — #1257). Merging that
      // entry would create a phantom room-bare "DM" alongside the
      // occupant-keyed conversation and double-count unread — skip it.
      // Deliberately WITHOUT rememberInboxAccountedMessage: the live
      // arrival must still increment the occupant conversation once
      // (unread for offline-missed PMs stays server-unattributable
      // until #1257 teaches the inbox about occupants). Also purge any
      // phantom room-bare conversation an earlier session created
      // before the room became known.
      if (xmppClient.value?.isKnownMucRoom?.(bare)) {
        merged.delete(bare);
        continue;
      }
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
        const bare = barePeerJid(conversation.partner);
        // #1256: never presence-subscribe to a MUC room bare JID (the
        // server projects MUC PMs under the room until #1257).
        if (xmppClient.value?.isKnownMucRoom?.(bare)) continue;
        void currentClient.subscribeToPeerPresence(bare).catch(() => undefined);
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
    const bare = conversationKeyFor(peerJid);
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
    const bare = conversationKeyFor(peerJid);
    // #1256: an initiated MUC-PM conversation gets the same provenance
    // metadata as a received one, and no presence subscribe is sent —
    // the PresenceManager would bare-fold the occupant JID and target
    // the room bare JID.
    const isOccupant = bare.includes("/");
    if (isOccupant) {
      ensureConversation(bare, mucPmDisplayName(bare), barePeerJid(bare));
    } else {
      ensureConversation(bare);
    }
    activePeerJid.value = bare;
    hydratePeerDmCallActivity(bare);
    if (isOccupant) return;
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
    // XEP-0280 restamp pass: the dispatch exists only to upgrade the
    // already-rendered row's timestamp — the direct copy already
    // updated the conversation and unread count.
    if (msg.timestampRefreshOnly) return;
    // XEP-0045 §7.5 (#1256): MUC PMs file under the full occupant JID —
    // never the room bare JID, where a reply would broadcast. The
    // displayed name carries room provenance ("nick (room)") so a room
    // occupant's self-chosen nick can never render byte-identical to a
    // real account's DM entry (nick-spoofing hardening).
    const bare = msg.mucPm ? msg.peerJid : barePeerJid(msg.peerJid);
    const mucPmUsername = msg.mucPm ? mucPmDisplayName(msg.peerJid) : undefined;
    const existing = ensureConversation(bare, mucPmUsername, msg.mucPm ? barePeerJid(msg.peerJid) : undefined);
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
        // #1256: occupant rows have no presence identity of their own —
        // subscribing would bare-fold to the room JID.
        if (c.mucPm || c.peerJid.includes("/")) continue;
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
    activeConversationScope,
    hasUnread,
    totalUnreadCount,
    openDm,
    closeDm,
    forgetPeer,
    hydrateFromInbox,
    onInboxPush,
    markRead,
    receiveIncomingDm,
    updatePresence,
  };
}
