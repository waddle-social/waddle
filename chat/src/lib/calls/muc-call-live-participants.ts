import { map } from "nanostores";
import { fullJidIdentityKey } from "@/lib/xmpp/jid";
import { normalizeMucCallRoomJid } from "./muc-call-presence";

/**
 * Source-of-truth projection of LiveKit's `RoomEvent.ParticipantConnected`
 * / `ParticipantDisconnected` stream for the MUC room our local
 * `$callState.active` slot is currently bound to.
 *
 * Why a separate store from `$mucCallParticipants`:
 *
 * - `$mucCallParticipants` is derived from inbound XEP-0272 Muji
 *   presence (now kept honest server-side by the LK→MUC webhook
 *   bridge). It is the right source for **rooms we are not in** —
 *   every chat client sees the same Muji broadcast and the indicator
 *   must agree across observers.
 * - `$mucCallLiveParticipants` is the LiveKit SDK's first-party
 *   participant set for the **room we ARE in**. LK fires
 *   `participantConnected` / `participantDisconnected` within seconds
 *   of the SFU's truth changing, often before XMPP server-side ping
 *   detects a dead resource; using it for the in-call view eliminates
 *   the ghost-while-in-call symptom independent of the webhook
 *   round-trip latency.
 *
 * Shape: `{ "<roomJid>": ["<participant-full-jid>", …] }`. Empty
 * arrays and missing rooms are equivalent — there is no participant
 * advertisement for a room we are not currently connected to via
 * LiveKit, so the absence of an entry means "fall back to the Muji
 * view." Identities are normalized via `fullJidIdentityKey` for
 * case-insensitive equality with the rest of the call layer.
 */
export const $mucCallLiveParticipants = map<Record<string, string[]>>({});

/**
 * Replace the entire participant set for `roomJid` with the supplied
 * identities. Used at LiveKit `Connected` time to seed the initial
 * remote-participant snapshot — including peers that joined before
 * our listeners attached.
 */
export function setLiveCallParticipants(
  roomJid: string,
  identities: ReadonlyArray<string>,
): void {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized) return;
  const deduped = Array.from(
    new Set(identities.map(fullJidIdentityKey).filter(Boolean)),
  );
  if (deduped.length === 0) {
    clearLiveCallParticipants(normalized);
    return;
  }
  $mucCallLiveParticipants.setKey(normalized, deduped);
}

/**
 * Add a single participant to `roomJid`'s live set. Idempotent:
 * re-adding an already-present identity is a no-op.
 */
export function addLiveCallParticipant(roomJid: string, identity: string): void {
  const normalized = normalizeMucCallRoomJid(roomJid);
  const normalizedIdentity = fullJidIdentityKey(identity);
  if (!normalized || !normalizedIdentity) return;
  const current = $mucCallLiveParticipants.get()[normalized] ?? [];
  if (current.includes(normalizedIdentity)) return;
  $mucCallLiveParticipants.setKey(normalized, [...current, normalizedIdentity]);
}

/**
 * Drop a single participant from `roomJid`'s live set. Idempotent:
 * removing an unknown identity is a no-op. When the last identity is
 * removed the room entry is deleted so `useRoomHasActiveCall`'s
 * fallback logic re-engages on the Muji-derived view.
 */
export function removeLiveCallParticipant(roomJid: string, identity: string): void {
  const normalized = normalizeMucCallRoomJid(roomJid);
  const normalizedIdentity = fullJidIdentityKey(identity);
  if (!normalized || !normalizedIdentity) return;
  const current = $mucCallLiveParticipants.get()[normalized] ?? [];
  if (!current.includes(normalizedIdentity)) return;
  const next = current.filter((existing) => existing !== normalizedIdentity);
  if (next.length === 0) {
    clearLiveCallParticipants(normalized);
    return;
  }
  $mucCallLiveParticipants.setKey(normalized, next);
}

/**
 * Drop every tracked identity for `roomJid`. Called on
 * `engine.disconnect()` so the room's UI reverts to the Muji-derived
 * (server-bridged) view without a stale LK snapshot lingering.
 */
export function clearLiveCallParticipants(roomJid: string): void {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized) return;
  const all = { ...$mucCallLiveParticipants.get() };
  if (!(normalized in all)) return;
  delete all[normalized];
  $mucCallLiveParticipants.set(all);
}

/**
 * Wipe every room's live participant snapshot. Used by logout /
 * connection teardown so a fresh login doesn't see stale indicators.
 */
export function clearAllLiveCallParticipants(): void {
  $mucCallLiveParticipants.set({});
}
