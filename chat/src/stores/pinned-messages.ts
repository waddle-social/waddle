// Per-room pinned-message state for #414. Keyed by room bare JID; each
// room's value is the typed pin entries returned by
// `xmpp.fetch_room_pins(...)` plus a derived stanza-id set for fast
// `MessageCard.isPinned` lookup.
//
// The store is hydrated on room entry and mutated live by the
// `pin-event` observer in the chat-app-controller. See
// `services/pinned-messages.ts` for the orchestration.

import { atom } from "nanostores";

import type { WasmPinEntry } from "@/lib/xmpp/wasm-types";

export interface PinnedRoomState {
  /** Pin entries in pin-time-desc order, mirroring server output. */
  entries: WasmPinEntry[];
  /** Derived: stanza-ids of pinned messages for cheap presence check. */
  stanzaIds: Set<string>;
  /** True once the initial fetch has completed (success or empty). */
  hydrated: boolean;
}

export type PinnedRoomMap = Map<string, PinnedRoomState>;

/** Empty state for a room that hasn't been hydrated yet. */
export const emptyPinnedRoomState = (): PinnedRoomState => ({
  entries: [],
  stanzaIds: new Set(),
  hydrated: false,
});

/** Reactive map: roomJid → PinnedRoomState. Replaced on every change. */
export const $pinnedRooms = atom<PinnedRoomMap>(new Map());

/** Replace one room's state and notify subscribers. */
export function setPinnedRoom(roomJid: string, state: PinnedRoomState): void {
  const next = new Map($pinnedRooms.get());
  next.set(roomJid, state);
  $pinnedRooms.set(next);
}

/** Hydrate the room with the full server-returned entries list. */
export function hydratePinnedRoom(roomJid: string, entries: WasmPinEntry[]): void {
  setPinnedRoom(roomJid, {
    entries,
    stanzaIds: new Set(entries.map((e) => e.target_stanza_id)),
    hydrated: true,
  });
}

/** Apply a pin-event update to a hydrated room. No-op if not hydrated:
 * the next room-entry will hydrate from scratch. */
export function applyPinEvent(
  roomJid: string,
  event: { action: "pinned" | "unpinned"; target_stanza_id: string; entry?: WasmPinEntry },
): void {
  const current = $pinnedRooms.get().get(roomJid);
  if (!current || !current.hydrated) return;
  let entries = current.entries;
  if (event.action === "pinned") {
    const filtered = entries.filter((e) => e.target_stanza_id !== event.target_stanza_id);
    entries = event.entry ? [event.entry, ...filtered] : filtered;
  } else {
    entries = entries.filter((e) => e.target_stanza_id !== event.target_stanza_id);
  }
  setPinnedRoom(roomJid, {
    entries,
    stanzaIds: new Set(entries.map((e) => e.target_stanza_id)),
    hydrated: true,
  });
}

/** Cheap O(1) presence check for `MessageCard.isPinned`. */
export function isPinnedStanza(roomJid: string, stanzaId: string | undefined): boolean {
  if (!stanzaId) return false;
  const state = $pinnedRooms.get().get(roomJid);
  return state?.stanzaIds.has(stanzaId) ?? false;
}

/** Drop a room's state — used on room destroy or sign-out. */
export function clearPinnedRoom(roomJid: string): void {
  const next = new Map($pinnedRooms.get());
  if (next.delete(roomJid)) {
    $pinnedRooms.set(next);
  }
}
