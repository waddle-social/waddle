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
const $mucPreparingParticipants = map<Record<string, string[]>>({});

export function normalizeMucCallRoomJid(roomJid: string): string {
  return roomJid.split("/")[0]?.trim().toLowerCase() ?? "";
}

function mucCallParticipantsForRoom(roomJid: string): string[] {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized) return [];
  return $mucCallParticipants.get()[normalized] ?? [];
}

export function mucCallParticipantCounts(
  participants: Record<string, string[]>,
): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const [roomJid, nicks] of Object.entries(participants)) {
    const normalized = normalizeMucCallRoomJid(roomJid);
    if (!normalized) continue;
    counts[normalized] = (counts[normalized] ?? 0) + nicks.length;
  }
  return counts;
}

export function clearMucCallParticipant(roomJid: string, nick: string): void {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized || !nick) return;
  const current = $mucCallParticipants.get()[normalized] ?? [];
  if (!current.includes(nick)) return;
  const next = current.filter((n) => n !== nick);
  if (next.length === 0) {
    const all = { ...$mucCallParticipants.get() };
    delete all[normalized];
    $mucCallParticipants.set(all);
  } else {
    $mucCallParticipants.setKey(normalized, next);
  }
}

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
  const roomJid = normalizeMucCallRoomJid(presence.from.slice(0, slash));
  const nick = presence.from.slice(slash + 1);
  if (!roomJid || !nick) return;

  const wantsActive =
    presence.presence_type !== "unavailable" &&
    presence.muji?.active === true;

  // XEP-0272 §Joining: a preparing-only presence echoed back from
  // the MUC is the signal a waiting client uses to proceed to its
  // content-declaring presence. Fire any registered one-shot
  // listener BEFORE we touch the participant store so the
  // beginMucCall flow unblocks deterministically.
  if (presence.muji?.preparing && presence.muji?.active === false) {
    addPreparingParticipant(roomJid, nick);
    notifyPrepareEcho(roomJid, nick);
  } else {
    removePreparingParticipant(roomJid, nick);
  }

  const current = mucCallParticipantsForRoom(roomJid);
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
  return mucCallParticipantsForRoom(roomJid).length;
}

/**
 * Forget every tracked participant. Called on logout / disconnect
 * so a fresh login doesn't see stale "in call" indicators.
 */
export function clearMucCallParticipants(): void {
  $mucCallParticipants.set({});
  $mucPreparingParticipants.set({});
  // Drop any pending preparing-echo waiters too — they'd otherwise
  // resolve when the next presence echo arrives after reconnect,
  // which would race the new login's call setup.
  rejectPrepareEchoWaiters(
    new Error("Muji preparing presence wait cancelled while clearing call state"),
  );
  rejectNoOtherPreparingWaiters(
    new Error("Muji preparing participant wait cancelled while clearing call state"),
  );
}

export function cancelMucCallPreparationWaiters(
  roomJid: string,
  nick: string,
  err: Error,
): void {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized || !nick) return;
  rejectPrepareEchoWaiter(`${normalized}/${nick}`, err);
  rejectNoOtherPreparingWaiter(`${normalized}/${nick}`, err);
}

function addPreparingParticipant(roomJid: string, nick: string): void {
  const current = $mucPreparingParticipants.get()[roomJid] ?? [];
  if (current.includes(nick)) return;
  $mucPreparingParticipants.setKey(roomJid, [...current, nick]);
  notifyPreparingChanged(roomJid);
}

function removePreparingParticipant(roomJid: string, nick: string): void {
  const current = $mucPreparingParticipants.get()[roomJid] ?? [];
  if (!current.includes(nick)) return;
  const next = current.filter((n) => n !== nick);
  if (next.length === 0) {
    const all = { ...$mucPreparingParticipants.get() };
    delete all[roomJid];
    $mucPreparingParticipants.set(all);
  } else {
    $mucPreparingParticipants.setKey(roomJid, next);
  }
  notifyPreparingChanged(roomJid);
}

/**
 * Pending listeners awaiting a XEP-0272 §Joining preparing-presence
 * echo. Keyed by `<room-jid>/<nick>` so two calls in different rooms
 * (or with different nicks) get distinct one-shot resolvers.
 *
 * The XEP MANDATES that a client wait for the MUC to rebroadcast
 * its preparing presence before proceeding to the content-declaring
 * presence. Without an explicit wait, multiple racing clients can
 * miss each other's preparing element and fail the §Joining
 * coordination protocol. Waddle's SFU model doesn't actually need
 * peer consensus (the mixer is the focus), but conformance is
 * conformance — we wait.
 */
const prepareEchoListeners = new Map<
  string,
  {
    resolve: () => void;
    reject: (err: Error) => void;
    timer: ReturnType<typeof setTimeout>;
  }
>();
const noOtherPreparingListeners = new Map<
  string,
  {
    roomJid: string;
    selfNick: string;
    resolve: () => void;
    reject: (err: Error) => void;
    timer: ReturnType<typeof setTimeout>;
  }
>();

function notifyPrepareEcho(roomJid: string, nick: string): void {
  const key = `${roomJid}/${nick}`;
  const listener = prepareEchoListeners.get(key);
  if (listener) {
    prepareEchoListeners.delete(key);
    clearTimeout(listener.timer);
    listener.resolve();
  }
}

function rejectPrepareEchoWaiters(err: Error): void {
  for (const listener of prepareEchoListeners.values()) {
    clearTimeout(listener.timer);
    listener.reject(err);
  }
  prepareEchoListeners.clear();
}

function rejectPrepareEchoWaiter(key: string, err: Error): void {
  const listener = prepareEchoListeners.get(key);
  if (!listener) return;
  prepareEchoListeners.delete(key);
  clearTimeout(listener.timer);
  listener.reject(err);
}

function hasOtherPreparing(roomJid: string, selfNick: string): boolean {
  return ($mucPreparingParticipants.get()[roomJid] ?? []).some((nick) => nick !== selfNick);
}

function notifyPreparingChanged(roomJid: string): void {
  for (const [key, listener] of noOtherPreparingListeners) {
    if (listener.roomJid !== roomJid) continue;
    if (hasOtherPreparing(listener.roomJid, listener.selfNick)) continue;
    noOtherPreparingListeners.delete(key);
    clearTimeout(listener.timer);
    listener.resolve();
  }
}

function rejectNoOtherPreparingWaiters(err: Error): void {
  for (const listener of noOtherPreparingListeners.values()) {
    clearTimeout(listener.timer);
    listener.reject(err);
  }
  noOtherPreparingListeners.clear();
}

function rejectNoOtherPreparingWaiter(key: string, err: Error): void {
  const listener = noOtherPreparingListeners.get(key);
  if (!listener) return;
  noOtherPreparingListeners.delete(key);
  clearTimeout(listener.timer);
  listener.reject(err);
}

/**
 * Wait for the MUC to rebroadcast our preparing-presence echo for
 * `nick` in `roomJid`, then resolve. If no echo arrives within
 * `timeoutMs`, reject: XEP-0272 makes the echoed preparing
 * presence part of the joining precondition, and proceeding without
 * it can leave local UI state ahead of room-visible presence state.
 *
 * Caller must register the listener BEFORE emitting the preparing
 * presence to avoid races where the echo arrives before
 * `applyMucCallPresence` has a listener to fire.
 */
export function awaitPreparingEcho(
  roomJid: string,
  nick: string,
  timeoutMs: number,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const normalized = normalizeMucCallRoomJid(roomJid);
    const key = `${normalized}/${nick}`;
    // Replacing an existing listener silently is fine — there's
    // only ever one preparing-echo waiter per (room, nick) at a
    // time because beginMucCall serialises through the call store.
    const previous = prepareEchoListeners.get(key);
    if (previous) {
      clearTimeout(previous.timer);
      previous.reject(new Error(`Replaced pending Muji preparing presence echo waiter in ${normalized}`));
    }
    const timer = setTimeout(() => {
      if (prepareEchoListeners.get(key)) {
        prepareEchoListeners.delete(key);
        reject(new Error(`Timed out waiting for Muji preparing presence echo in ${normalized}`));
      }
    }, timeoutMs);
    prepareEchoListeners.set(key, { resolve, reject, timer });
  });
}

export function awaitNoOtherPreparing(
  roomJid: string,
  selfNick: string,
  timeoutMs: number,
): Promise<void> {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!hasOtherPreparing(normalized, selfNick)) {
    return Promise.resolve();
  }
  return new Promise((resolve, reject) => {
    const key = `${normalized}/${selfNick}`;
    const previous = noOtherPreparingListeners.get(key);
    if (previous) {
      clearTimeout(previous.timer);
      previous.reject(new Error(`Replaced pending Muji preparation waiter in ${normalized}`));
    }
    const timer = setTimeout(() => {
      if (noOtherPreparingListeners.get(key)) {
        noOtherPreparingListeners.delete(key);
        reject(new Error(`Timed out waiting for other Muji participants to finish preparing in ${normalized}`));
      }
    }, timeoutMs);
    noOtherPreparingListeners.set(key, {
      roomJid: normalized,
      selfNick,
      resolve,
      reject,
      timer,
    });
  });
}
