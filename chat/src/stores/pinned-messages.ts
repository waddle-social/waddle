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

interface PinnedRoomState {
  /** Pin entries in pin-time-desc order, mirroring server output. */
  entries: WasmPinEntry[];
  /** Derived: stanza-ids of pinned messages for cheap presence check. */
  stanzaIds: Set<string>;
  /** True once the initial fetch has completed (success or empty). */
  hydrated: boolean;
}

/** Reactive map: roomJid → PinnedRoomState. Replaced on every change. */
export const $pinnedRooms = atom<Map<string, PinnedRoomState>>(new Map());

type PinUpdate =
  | { action: "pinned"; target_stanza_id: string; entry?: WasmPinEntry }
  | { action: "unpinned"; target_stanza_id: string };

/** Pre-hydration event queue: events that arrive while
 * `fetchRoomPins` is in flight are buffered here and replayed in
 * order once `hydratePinnedRoom` lands. Without this, an admin's
 * pin/unpin issued during initial hydration was silently dropped. */
const pendingUpdates = new Map<string, PinUpdate[]>();

/** Replace one room's state and notify subscribers. */
function setPinnedRoom(roomJid: string, state: PinnedRoomState): void {
  const next = new Map($pinnedRooms.get());
  next.set(roomJid, state);
  $pinnedRooms.set(next);
}

/** Hydrate the room with the full server-returned entries list. Any
 * pin/unpin events that arrived during the in-flight hydration are
 * replayed on top of the canonical list, so the final state reflects
 * both the snapshot and any concurrent mutations. */
export function hydratePinnedRoom(roomJid: string, entries: WasmPinEntry[]): void {
  setPinnedRoom(roomJid, {
    entries,
    stanzaIds: new Set(entries.map((e) => e.target_stanza_id)),
    hydrated: true,
  });
  const pending = pendingUpdates.get(roomJid);
  if (pending && pending.length > 0) {
    pendingUpdates.delete(roomJid);
    for (const update of pending) {
      applyPinEvent(roomJid, update);
    }
  }
}

/** Apply a pin-event update. If the room hasn't been hydrated yet,
 * the event is queued and replayed once `hydratePinnedRoom` lands —
 * preventing the dropped-event race when an admin pins/unpins during
 * initial fetch. */
export function applyPinEvent(roomJid: string, event: PinUpdate): void {
  const current = $pinnedRooms.get().get(roomJid);
  if (!current || !current.hydrated) {
    const queue = pendingUpdates.get(roomJid) ?? [];
    queue.push(event);
    pendingUpdates.set(roomJid, queue);
    return;
  }
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

