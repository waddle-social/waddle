import { computed, ref, watch, type Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient, DmConversation, LiveDmMessage, PresenceUpdateEvent } from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp-client";

function peerUsername(peerJid: string): string {
  return barePeerJid(peerJid).split("@")[0] ?? peerJid;
}

function sortByRecent(conversations: DmConversation[]): DmConversation[] {
  return [...conversations].sort((a, b) => {
    const at = a.lastMessageAt ? Date.parse(a.lastMessageAt) : 0;
    const bt = b.lastMessageAt ? Date.parse(b.lastMessageAt) : 0;
    return bt - at;
  });
}

export function useDmConversations(
  session: Ref<WaddleSession | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
) {
  const conversations = ref<DmConversation[]>([]);
  const activePeerJid = ref<string | null>(null);
  const presenceByJid = ref<Record<string, DmConversation["presenceShow"]>>({});

  const hasUnread = computed(() => conversations.value.some((c) => c.unreadCount > 0));

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
        conversations: conversations.value,
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
    };
    conversations.value = sortByRecent([...conversations.value, created]);
    return created;
  }

  function markRead(peerJid: string) {
    const bare = barePeerJid(peerJid);
    conversations.value = conversations.value.map((c) => (
      c.peerJid === bare ? { ...c, unreadCount: 0 } : c
    ));
  }

  async function openDm(peerJid: string) {
    const bare = barePeerJid(peerJid);
    ensureConversation(bare);
    activePeerJid.value = bare;
    markRead(bare);
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
    const shouldIncrementUnread = msg.fromJid !== barePeerJid(session.value?.jid ?? "")
      && activePeerJid.value !== bare;
    conversations.value = sortByRecent(conversations.value.map((c) => (
      c.peerJid === bare
        ? {
            ...c,
            peerUsername: existing.peerUsername || peerUsername(bare),
            lastMessageBody: msg.body,
            lastMessageAt: msg.createdAt,
            unreadCount: shouldIncrementUnread ? c.unreadCount + 1 : c.unreadCount,
            presenceShow: presenceByJid.value[bare] ?? c.presenceShow,
          }
        : c
    )));
  }

  function updatePresence(event: PresenceUpdateEvent) {
    const bare = barePeerJid(event.bareJid);
    presenceByJid.value = { ...presenceByJid.value, [bare]: event.show };
    conversations.value = conversations.value.map((c) => (
      c.peerJid === bare ? { ...c, presenceShow: event.show } : c
    ));
  }

  watch(
    () => session.value?.jid,
    () => {
      conversations.value = [];
      activePeerJid.value = null;
      presenceByJid.value = {};
      restore();
      for (const c of conversations.value) {
        xmppClient.value?.subscribeToPeerPresence(c.peerJid);
      }
    },
    { immediate: true },
  );

  watch([conversations, activePeerJid], persist, { deep: true });

  return {
    conversations,
    activePeerJid,
    hasUnread,
    openDm,
    closeDm,
    markRead,
    receiveIncomingDm,
    updatePresence,
  };
}
