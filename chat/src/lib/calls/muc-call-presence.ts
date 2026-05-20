import { map } from "nanostores";

/**
 * Per-room map of nicks advertising XEP-0272 Muji presence with
 * active `<content/>` children — i.e. occupants currently in the
 * room's group call.
 *
 * Drives the channel-header "N in call" indicator AND the per-row
 * sidebar badge in `TopicsPanel.vue`.
 *
 * Updated by `applyMucCallPresence`, called from the chat-side
 * `set_on_presence` wrapper whenever an occupant's presence carries
 * a Muji extension. Per XEP-0272 §Leaving, the absence of the
 * `<muji/>` element is the leave marker — we drop the nick when an
 * available presence arrives without one. `<preparing/>`-only
 * Muji (the two-phase join sentinel) is NOT treated as active.
 *
 * Shape: `{ "room@muc.host": ["alice", "bob"] }`. Arrays kept
 * sorted by insertion order so the UI doesn't reshuffle on every
 * change.
 */
export const $mucCallParticipants = map<Record<string, string[]>>({});

/**
 * Apply an inbound presence update to the participants store. The
 * cases (XEP-0272 §Joining and §Leaving):
 * - Available presence + `muji.active === true` → add nick.
 * - Available presence + `muji.preparing === true` (but not active)
 *   → no-op; preparing alone does not count as in-call.
 * - Available presence WITHOUT `muji` → remove nick (XEP-0272
 *   §Leaving: absence of the element is the leave marker).
 * - Unavailable presence → remove nick (occupant left the room).
 *
 * Robust against duplicate / replayed presences: re-adding an
 * already-present nick is a no-op, removing a never-added one is
 * also a no-op.
 */
export function applyMucCallPresence(
  presence: {
    from?: string;
    presence_type?: string;
    muji?: { preparing: boolean; active: boolean };
  },
): void {
  if (!presence.from) return;
  const slash = presence.from.indexOf("/");
  if (slash < 0) return;
  const roomJid = presence.from.slice(0, slash);
  const nick = presence.from.slice(slash + 1);
  if (!nick) return;

  const wantsActive =
    presence.presence_type !== "unavailable" &&
    presence.muji?.active === true;

  const current = $mucCallParticipants.get()[roomJid] ?? [];
  const has = current.includes(nick);

  if (wantsActive && !has) {
    $mucCallParticipants.setKey(roomJid, [...current, nick]);
  } else if (!wantsActive && has) {
    const next = current.filter((n) => n !== nick);
    if (next.length === 0) {
      // Drop the room key entirely so consumers can read
      // `$mucCallParticipants.get()[room] ?? []` and treat absence
      // as "nobody in call" — same as the initial state.
      const all = { ...$mucCallParticipants.get() };
      delete all[roomJid];
      $mucCallParticipants.set(all);
    } else {
      $mucCallParticipants.setKey(roomJid, next);
    }
  }
}

/**
 * Number of nicks currently advertising the call in `roomJid`.
 * Convenience for components that don't need the full list.
 */
export function mucCallParticipantCount(roomJid: string): number {
  return ($mucCallParticipants.get()[roomJid] ?? []).length;
}

/**
 * Forget every tracked participant. Called on logout / disconnect
 * so a fresh login doesn't see stale "in call" indicators.
 */
export function clearMucCallParticipants(): void {
  $mucCallParticipants.set({});
}
