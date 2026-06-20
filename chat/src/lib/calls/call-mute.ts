import { map } from "nanostores";
import { fullJidIdentityKey } from "@/lib/xmpp/jid";
import { normalizeMucCallRoomJid } from "./muc-call-presence";

/**
 * Per-room set of call participants advertising a muted microphone
 * (`urn:waddle:in-call:0` `<muted/>` presence state, #1030). Keyed by
 * normalized room JID, then by `fullJidIdentityKey` of the occupant's
 * real full JID — the SAME identity key the call roster and tiles use —
 * so the UI can join "who is muted" against the roster rows and camera
 * tiles without a second lookup table.
 *
 * This is the AUTHORITATIVE remote-mute view: it replaces LiveKit's own
 * track-mute signalling (the non-XMPP control path) for displaying other
 * participants' mute state. The local participant's own mute stays sourced
 * from the LiveKit mic state (`$callMicEnabled`) — this map only carries
 * remote occupants' presence-advertised mute.
 *
 * Fed by `applyMutePresence` from the chat-side presence wrapper: a MUC
 * occupant's presence carries the `<muted/>` `<in-call>` child alongside
 * `<muji/>` when muted, and omits it when unmuted. Leaving the call or the
 * room clears the entry (unmuted / unavailable presence).
 *
 * Shape: `{ "room@muc.host": ["alice@host/web", "bob@host/phone"] }`,
 * each list sorted for stable render order.
 */
export const $mucMutedParticipants = map<Record<string, string[]>>({});

type MuteAction =
  | { kind: "set"; roomJid: string; identityKey: string; muted: boolean }
  | { kind: "clear-room"; roomJid: string }
  | { kind: "clear-all" };

/**
 * Pure reducer over the room → identity-keys muted record. Never mutates
 * `current`, and returns the SAME `current` reference for no-op updates
 * (idempotent set, invalid room/id, clearing what's already absent) so a
 * `$mucMutedParticipants.set(...)` round-trip skips the store notification
 * and avoids spurious rerenders. Pruning an empty room keeps the record
 * free of stale keys so `Object.keys` reflects rooms with live mutes.
 */
export function reduceMutedParticipants(
  current: Record<string, string[]>,
  action: MuteAction,
): Record<string, string[]> {
  if (action.kind === "clear-all") {
    return Object.keys(current).length === 0 ? current : {};
  }

  const room = normalizeMucCallRoomJid(action.roomJid);
  if (!room) return current;

  if (action.kind === "clear-room") {
    if (!(room in current)) return current;
    const next = cloneRecord(current);
    delete next[room];
    return next;
  }

  const { identityKey, muted } = action;
  if (!identityKey) return current;
  const existing = current[room] ?? [];
  const has = existing.includes(identityKey);

  if (muted) {
    if (has) return current;
    const next = cloneRecord(current);
    next[room] = [...existing, identityKey].sort();
    return next;
  }

  if (!has) return current;
  const next = cloneRecord(current);
  const filtered = existing.filter((key) => key !== identityKey);
  if (filtered.length === 0) delete next[room];
  else next[room] = filtered;
  return next;
}

function cloneRecord(current: Record<string, string[]>): Record<string, string[]> {
  const next: Record<string, string[]> = {};
  for (const [room, keys] of Object.entries(current)) next[room] = [...keys];
  return next;
}

/** Minimal inbound presence shape this module reads. */
type MutePresence = {
  from?: string;
  presence_type?: string;
  muc_jid?: string | null;
  muted?: boolean;
};

/**
 * Map an inbound MUC presence to a mute `set` action, or `null` when it
 * carries no usable occupant identity. The occupant's real full JID
 * (`muc_jid`) is the join key; an anonymous presence without one is
 * ignored. An unavailable presence or a missing flag both unmute. Pure —
 * the store side effect lives in `applyMutePresence`.
 */
export function mutePresenceUpdate(
  presence: MutePresence,
): Extract<MuteAction, { kind: "set" }> | null {
  if (!presence.from) return null;
  const slash = presence.from.indexOf("/");
  if (slash < 0) return null;
  const roomJid = normalizeMucCallRoomJid(presence.from.slice(0, slash));
  if (!roomJid) return null;
  const identityKey = fullJidIdentityKey(presence.muc_jid);
  if (!identityKey) return null;
  const muted = presence.presence_type !== "unavailable" && presence.muted === true;
  return { kind: "set", roomJid, identityKey, muted };
}

/** Apply an inbound presence's mute state to the store. */
export function applyMutePresence(presence: MutePresence): void {
  const action = mutePresenceUpdate(presence);
  if (!action) return;
  $mucMutedParticipants.set(reduceMutedParticipants($mucMutedParticipants.get(), action));
}

/** Identity keys muted in `roomJid` (empty set if none). */
export function mutedKeysForRoom(
  roomJid: string,
  mutedByRoom: Record<string, readonly string[]> = $mucMutedParticipants.get(),
): Set<string> {
  const room = normalizeMucCallRoomJid(roomJid);
  if (!room) return new Set();
  return new Set(mutedByRoom[room] ?? []);
}

/** Forget every advertised mute (logout / disconnect). */
export function clearAllMuted(): void {
  $mucMutedParticipants.set({});
}
