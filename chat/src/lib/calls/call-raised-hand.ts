import { map } from "nanostores";
import { fullJidIdentityKey } from "@/lib/xmpp/jid";
import { normalizeMucCallRoomJid } from "./muc-call-presence";

/**
 * Per-room set of call participants advertising a raised hand
 * (`urn:waddle:in-call:0` presence state, #1029). Keyed by normalized
 * room JID, then by `fullJidIdentityKey` of the occupant's real full
 * JID — the SAME identity key the call roster and tiles use — so the
 * UI can join "who has a hand up" against the roster rows without a
 * second lookup table.
 *
 * Fed by `applyRaisedHandPresence` from the chat-side presence wrapper:
 * a MUC occupant's presence carries the raised-hand `<in-call>` child
 * alongside `<muji/>` when up, and omits it when down. Leaving the call
 * or the room clears the entry (lowered / unavailable presence).
 *
 * Shape: `{ "room@muc.host": ["alice@host/web", "bob@host/phone"] }`,
 * each list sorted for stable render order.
 */
export const $mucRaisedHands = map<Record<string, string[]>>({});

/**
 * This client's own raised-hand state per room. Decoupled from the
 * inbound `$mucRaisedHands` view so the control bar reflects the local
 * intent immediately and every outbound call presence re-emits the
 * current flag (presence is last-writer-wins, so a later video/media
 * presence must not silently drop a raised hand).
 */
export const $selfRaisedHand = map<Record<string, boolean>>({});

export type RaisedHandAction =
  | { kind: "set"; roomJid: string; identityKey: string; raised: boolean }
  | { kind: "clear-room"; roomJid: string }
  | { kind: "clear-all" };

/**
 * Pure reducer over the room → identity-keys raised-hand record.
 * Never mutates `current`. Pruning an empty room keeps the record
 * free of stale keys so `Object.keys` reflects rooms with live hands.
 */
export function reduceRaisedHands(
  current: Record<string, readonly string[]>,
  action: RaisedHandAction,
): Record<string, string[]> {
  if (action.kind === "clear-all") return {};

  const room = normalizeMucCallRoomJid(action.roomJid);
  const next = cloneRecord(current);
  if (!room) return next;

  if (action.kind === "clear-room") {
    delete next[room];
    return next;
  }

  const { identityKey, raised } = action;
  if (!identityKey) return next;
  const existing = next[room] ?? [];
  const has = existing.includes(identityKey);

  if (raised) {
    if (has) return next;
    next[room] = [...existing, identityKey].sort();
    return next;
  }

  if (!has) return next;
  const filtered = existing.filter((key) => key !== identityKey);
  if (filtered.length === 0) delete next[room];
  else next[room] = filtered;
  return next;
}

function cloneRecord(
  current: Record<string, readonly string[]>,
): Record<string, string[]> {
  const next: Record<string, string[]> = {};
  for (const [room, keys] of Object.entries(current)) next[room] = [...keys];
  return next;
}

/** Minimal inbound presence shape this module reads. */
export type RaisedHandPresence = {
  from?: string;
  presence_type?: string;
  muc_jid?: string | null;
  hand_raised?: boolean;
};

/**
 * Map an inbound MUC presence to a raised-hand `set` action, or `null`
 * when it carries no usable occupant identity. The occupant's real full
 * JID (`muc_jid`) is the join key; an anonymous presence without one is
 * ignored. An unavailable presence or a missing flag both lower the
 * hand. Pure — the store side effect lives in `applyRaisedHandPresence`.
 */
export function raisedHandPresenceUpdate(
  presence: RaisedHandPresence,
): Extract<RaisedHandAction, { kind: "set" }> | null {
  if (!presence.from) return null;
  const slash = presence.from.indexOf("/");
  if (slash < 0) return null;
  const roomJid = normalizeMucCallRoomJid(presence.from.slice(0, slash));
  if (!roomJid) return null;
  const identityKey = fullJidIdentityKey(presence.muc_jid);
  if (!identityKey) return null;
  const raised = presence.presence_type !== "unavailable" && presence.hand_raised === true;
  return { kind: "set", roomJid, identityKey, raised };
}

/** Apply an inbound presence's raised-hand state to the store. */
export function applyRaisedHandPresence(presence: RaisedHandPresence): void {
  const action = raisedHandPresenceUpdate(presence);
  if (!action) return;
  $mucRaisedHands.set(reduceRaisedHands($mucRaisedHands.get(), action));
}

/** Identity keys with a raised hand in `roomJid` (empty set if none). */
export function raisedHandKeysForRoom(
  roomJid: string,
  raisedByRoom: Record<string, readonly string[]> = $mucRaisedHands.get(),
): Set<string> {
  const room = normalizeMucCallRoomJid(roomJid);
  if (!room) return new Set();
  return new Set(raisedByRoom[room] ?? []);
}

/** Whether this client currently advertises a raised hand in `roomJid`. */
export function selfRaisedHandFor(
  roomJid: string,
  selfByRoom: Record<string, boolean> = $selfRaisedHand.get(),
): boolean {
  const room = normalizeMucCallRoomJid(roomJid);
  return room ? selfByRoom[room] === true : false;
}

/** Set this client's own raised-hand intent for `roomJid`. */
export function setSelfRaisedHand(roomJid: string, raised: boolean): void {
  const room = normalizeMucCallRoomJid(roomJid);
  if (!room) return;
  if (raised) $selfRaisedHand.setKey(room, true);
  else {
    const next = { ...$selfRaisedHand.get() };
    delete next[room];
    $selfRaisedHand.set(next);
  }
}

/** Drop all raised-hand state for a room (e.g. when the local call ends). */
export function clearRaisedHandsForRoom(roomJid: string): void {
  $mucRaisedHands.set(
    reduceRaisedHands($mucRaisedHands.get(), { kind: "clear-room", roomJid }),
  );
  const room = normalizeMucCallRoomJid(roomJid);
  if (room && room in $selfRaisedHand.get()) {
    const next = { ...$selfRaisedHand.get() };
    delete next[room];
    $selfRaisedHand.set(next);
  }
}

/** Forget every raised hand (logout / disconnect). */
export function clearAllRaisedHands(): void {
  $mucRaisedHands.set({});
  $selfRaisedHand.set({});
}
