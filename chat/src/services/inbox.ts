/**
 * Inbox service layer.
 *
 * Manages inbox state (channel + thread level).
 * Vue composables consume this as plain reactive state via subscription.
 */
import type { InboxEntry } from "@/lib/xmpp/inbox";

/** Composite key for deduplication: partner + optional thread. */
function entryKey(entry: InboxEntry): string {
  return entry.thread ? `${entry.partner}::${entry.thread}` : entry.partner;
}

export interface InboxState {
  /** Channel-level entries keyed by room JID. */
  channels: Map<string, InboxEntry>;
  /** Thread-level entries keyed by `roomJid::threadId`. */
  threads: Map<string, InboxEntry>;
}

export function createInboxState(): InboxState {
  return { channels: new Map(), threads: new Map() };
}

/** Apply an inbox entry (from hydration or push) to state. Returns updated state. */
export function applyEntry(state: InboxState, entry: InboxEntry): InboxState {
  const key = entryKey(entry);
  if (entry.thread) {
    const next = new Map(state.threads);
    next.set(key, entry);
    return { ...state, threads: next };
  }
  const next = new Map(state.channels);
  next.set(key, entry);
  return { ...state, channels: next };
}

/** Apply a batch of entries. */
export function applyEntries(state: InboxState, entries: InboxEntry[]): InboxState {
  let s = state;
  for (const entry of entries) {
    s = applyEntry(s, entry);
  }
  return s;
}

/** Mark a channel or thread as read in state. */
export function markReadInState(
  state: InboxState,
  roomJid: string,
  threadId?: string,
): InboxState {
  if (threadId) {
    const key = `${roomJid}::${threadId}`;
    const existing = state.threads.get(key);
    if (!existing || existing.unread === 0) return state;
    const next = new Map(state.threads);
    next.set(key, { ...existing, unread: 0 });
    return { ...state, threads: next };
  }
  const existing = state.channels.get(roomJid);
  if (!existing || existing.unread === 0) return state;
  const next = new Map(state.channels);
  next.set(roomJid, { ...existing, unread: 0 });
  return { ...state, channels: next };
}

/** Get thread entries for a specific room, sorted by lastUpdated desc. */
export function threadsForRoom(state: InboxState, roomJid: string): InboxEntry[] {
  const result: InboxEntry[] = [];
  for (const entry of state.threads.values()) {
    if (entry.partner === roomJid) {
      result.push(entry);
    }
  }
  result.sort((a, b) => b.lastUpdated - a.lastUpdated);
  return result;
}
