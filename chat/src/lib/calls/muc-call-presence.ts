import { map } from "nanostores";

/**
 * Per-room map of nicks who have advertised the
 * `urn:waddle:muc-call:0` presence extension with `state='active'`.
 * Drives the channel-header "N in call" indicator and any future
 * presence-driven join affordances.
 *
 * Updated by `applyMucCallPresence`, called from the chat-side
 * `set_on_presence` wrapper whenever an occupant's presence carries
 * the extension. Switching to `state='inactive'` or sending an
 * `unavailable` presence removes the nick from the room's set.
 *
 * Shape: `{ "room@muc.host": ["alice", "bob"] }`. Arrays kept
 * sorted by insertion order so the UI doesn't reshuffle on every
 * change.
 */
export const $mucCallParticipants = map<Record<string, string[]>>({});

/**
 * Apply an inbound presence update to the participants store. The
 * cases:
 * - Available presence + `muc_call.state === "active"` → add nick.
 * - Available presence + `muc_call.state === "inactive"` → remove nick.
 * - Available presence without `muc_call` → remove nick (treat as
 *   "no longer advertising", since the user may have left the call
 *   without sending an explicit `inactive` if their client crashed).
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
    muc_call?: { state: "active" | "inactive"; call_id: string };
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
    presence.muc_call?.state === "active";

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
