// Per-conversation "last message the local user has viewed" anchor, stored in
// localStorage so the client can restore scroll position between sessions and
// render a "new messages" divider. This is a local UX concern distinct from
// XEP-0333 <displayed/> receipts, which communicate *other users'* read state.

const PREFIX = "waddle.chat.last-seen";

export function roomKey(channelId: string): string {
  return `${PREFIX}.room.${channelId}`;
}

export function dmKey(peerBareJid: string): string {
  return `${PREFIX}.dm.${peerBareJid}`;
}

/**
 * Key used by XEP-0490 MDS to persist the latest displayed stanza-id
 * received from another device of the same account. The chat id is
 * the bare JID of the chat (DM contact or MUC room) per XEP-0490 §3.
 * The local readers that own the "new messages" divider for the
 * corresponding conversation can look up this key in addition to
 * their conversation-scoped key.
 */
export function mdsChatKey(chatBareJid: string): string {
  return `${PREFIX}.mds.${chatBareJid}`;
}

export type MdsDisplayedState = {
  stanzaId: string;
  stanzaIdBy: string;
};

type MdsDisplayedStore = {
  current: MdsDisplayedState | null;
  pending: MdsDisplayedState[];
};

function storage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function setLastSeen(key: string, messageId: string): void {
  const s = storage();
  if (!s) return;
  try {
    s.setItem(key, messageId);
  } catch {
    // Quota exceeded or storage disabled; silently drop — the worst outcome is
    // that the divider shows up on the next visit instead of not at all.
  }
}

function getLastSeen(key: string): string | null {
  const s = storage();
  if (!s) return null;
  try {
    return s.getItem(key);
  } catch {
    return null;
  }
}

export function setMdsDisplayed(key: string, state: MdsDisplayedState): void {
  const normalized = normalizeMdsDisplayedState(state);
  if (!normalized) return;
  const s = storage();
  if (!s) return;
  try {
    s.setItem(key, JSON.stringify({ current: normalized, pending: [] }));
  } catch {
    // Quota exceeded or storage disabled; the XMPP state remains authoritative.
  }
}

export function getMdsDisplayed(key: string): MdsDisplayedState | null {
  return readMdsStore(key).current;
}

export function queueMdsDisplayed(key: string, state: MdsDisplayedState): void {
  const normalized = normalizeMdsDisplayedState(state);
  if (!normalized) return;
  const s = storage();
  if (!s) return;
  const store = readMdsStore(key);
  if (
    (store.current && sameMdsDisplayedState(store.current, normalized)) ||
    store.pending.some((candidate) => sameMdsDisplayedState(candidate, normalized))
  ) {
    return;
  }
  const pending = [...store.pending, normalized].slice(-10);
  try {
    s.setItem(key, JSON.stringify({ current: store.current, pending }));
  } catch {
    // Quota exceeded or storage disabled; the XMPP state remains authoritative.
  }
}

export function getMdsDisplayedCandidates(key: string): MdsDisplayedState[] {
  const store = readMdsStore(key);
  const candidates = store.current ? [store.current, ...store.pending] : store.pending;
  return candidates.filter((candidate, index) =>
    candidates.findIndex((other) => sameMdsDisplayedState(other, candidate)) === index
  );
}

function readMdsStore(key: string): MdsDisplayedStore {
  const raw = getLastSeen(key);
  if (!raw) return { current: null, pending: [] };
  try {
    const parsed = JSON.parse(raw) as
      | Partial<MdsDisplayedState>
      | Partial<MdsDisplayedStore>
      | null;
    const direct = normalizeMdsDisplayedState(parsed as Partial<MdsDisplayedState>);
    if (direct) return { current: direct, pending: [] };
    const current = normalizeMdsDisplayedState(
      (parsed as Partial<MdsDisplayedStore> | null)?.current ?? null,
    );
    const pending = Array.isArray((parsed as Partial<MdsDisplayedStore> | null)?.pending)
      ? ((parsed as Partial<MdsDisplayedStore>).pending ?? []).flatMap((state) => {
          const normalized = normalizeMdsDisplayedState(state);
          return normalized ? [normalized] : [];
        })
      : [];
    return { current, pending };
  } catch {
    return { current: null, pending: [] };
  }
}

function normalizeMdsDisplayedState(
  state: Partial<MdsDisplayedState> | null | undefined,
): MdsDisplayedState | null {
  const stanzaId = typeof state?.stanzaId === "string" ? state.stanzaId.trim() : "";
  const stanzaIdBy = typeof state?.stanzaIdBy === "string" ? state.stanzaIdBy.trim() : "";
  return stanzaId && stanzaIdBy ? { stanzaId, stanzaIdBy } : null;
}

function sameMdsDisplayedState(a: MdsDisplayedState, b: MdsDisplayedState): boolean {
  return a.stanzaId === b.stanzaId && a.stanzaIdBy === b.stanzaIdBy;
}
