// Per-room cache of full `TimelineMessage` bodies for pinned-panel
// rich preview. Keyed by (roomJid, target_stanza_id). Populated lazily
// when the panel opens (and on `applyPinEvent("pinned")` for entries
// not already in the channel timeline). Evicted on unpin.
//
// Lifecycle is gated by the same epoch counter as `$pinnedRooms` so
// that late MAM responses captured before logout cannot leak the
// previous session's data into a new login. See `pinned-messages.ts`
// for the parent rationale.

import { atom } from "nanostores";

import type { TimelineMessage } from "@/lib/chat-ui";

type PinnedBodyMap = Map<string, Map<string, TimelineMessage>>;

export const $pinnedMessageBodies = atom<PinnedBodyMap>(new Map());

let currentEpoch = 0;

export function pinnedMessageBodiesEpoch(): number {
  return currentEpoch;
}

export function cachePinnedMessageBody(
  roomJid: string,
  stanzaId: string,
  message: TimelineMessage,
  epoch: number = currentEpoch,
): void {
  if (epoch !== currentEpoch) return;
  const next: PinnedBodyMap = new Map($pinnedMessageBodies.get());
  const room = new Map(next.get(roomJid) ?? new Map());
  room.set(stanzaId, message);
  next.set(roomJid, room);
  $pinnedMessageBodies.set(next);
}

export function cachePinnedMessageBodies(
  roomJid: string,
  entries: Array<{ stanzaId: string; message: TimelineMessage }>,
  epoch: number = currentEpoch,
): void {
  if (epoch !== currentEpoch || entries.length === 0) return;
  const next: PinnedBodyMap = new Map($pinnedMessageBodies.get());
  const room = new Map(next.get(roomJid) ?? new Map());
  for (const { stanzaId, message } of entries) {
    room.set(stanzaId, message);
  }
  next.set(roomJid, room);
  $pinnedMessageBodies.set(next);
}

export function evictPinnedMessageBody(roomJid: string, stanzaId: string): void {
  const current = $pinnedMessageBodies.get().get(roomJid);
  if (!current?.has(stanzaId)) return;
  const next: PinnedBodyMap = new Map($pinnedMessageBodies.get());
  const room = new Map(current);
  room.delete(stanzaId);
  if (room.size === 0) {
    next.delete(roomJid);
  } else {
    next.set(roomJid, room);
  }
  $pinnedMessageBodies.set(next);
}

export function resetPinnedMessageBodies(): void {
  $pinnedMessageBodies.set(new Map());
  currentEpoch += 1;
}
