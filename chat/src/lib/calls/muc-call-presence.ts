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
const $mucActiveParticipants = map<Record<string, string[]>>({});
const $mucPreparingParticipants = map<Record<string, string[]>>({});
const PARTICIPANT_KEY_SEPARATOR = "\u0000";

export function normalizeMucCallRoomJid(roomJid: string): string {
  return roomJid.split("/")[0]?.trim().toLowerCase() ?? "";
}

function normalizeFullJid(jid?: string | null): string {
  return jid?.trim().toLowerCase() ?? "";
}

function preparingParticipantKey(nick: string, realJid?: string | null): string {
  const normalizedRealJid = normalizeFullJid(realJid);
  return normalizedRealJid ? `${nick}${PARTICIPANT_KEY_SEPARATOR}${normalizedRealJid}` : nick;
}

function preparingParticipantNick(key: string): string {
  return key.split(PARTICIPANT_KEY_SEPARATOR)[0] ?? key;
}

function preparingParticipantRealJid(key: string): string {
  return key.includes(PARTICIPANT_KEY_SEPARATOR)
    ? key.slice(key.indexOf(PARTICIPANT_KEY_SEPARATOR) + PARTICIPANT_KEY_SEPARATOR.length)
    : "";
}

const activeParticipantKey = preparingParticipantKey;
const activeParticipantNick = preparingParticipantNick;
const activeParticipantRealJid = preparingParticipantRealJid;

function samePreparingParticipant(key: string, nick: string, realJid?: string | null): boolean {
  if (preparingParticipantNick(key) !== nick) return false;
  const normalizedRealJid = normalizeFullJid(realJid);
  const keyRealJid = preparingParticipantRealJid(key);
  if (normalizedRealJid || keyRealJid) return normalizedRealJid === keyRealJid;
  return true;
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

export function clearMucCallParticipant(
  roomJid: string,
  nick: string,
  realJid?: string | null,
): void {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!normalized || !nick) return;
  removeActiveParticipant(normalized, nick, realJid, {
    includeAggregate: !realJid,
  });
  if (realJid) return;
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

function syncActiveParticipantsForRoom(roomJid: string): void {
  const activeOwners = $mucActiveParticipants.get()[roomJid] ?? [];
  const nicks = Array.from(new Set(activeOwners.map(activeParticipantNick).filter(Boolean)));
  if (nicks.length === 0) {
    const all = { ...$mucCallParticipants.get() };
    delete all[roomJid];
    $mucCallParticipants.set(all);
  } else {
    $mucCallParticipants.setKey(roomJid, nicks);
  }
}

function addActiveParticipant(roomJid: string, nick: string, realJid?: string | null): void {
  const current = $mucActiveParticipants.get()[roomJid] ?? [];
  const key = activeParticipantKey(nick, realJid);
  if (current.includes(key)) return;
  $mucActiveParticipants.setKey(roomJid, [...current, key]);
  syncActiveParticipantsForRoom(roomJid);
}

function removeActiveParticipant(
  roomJid: string,
  nick: string,
  realJid?: string | null,
  options: { includeAggregate?: boolean } = {},
): void {
  const current = $mucActiveParticipants.get()[roomJid] ?? [];
  const normalizedRealJid = normalizeFullJid(realJid);
  const next = current.filter((key) => {
    if (activeParticipantNick(key) !== nick) return true;
    if (normalizedRealJid) {
      if (activeParticipantRealJid(key) === normalizedRealJid) return false;
      return !(options.includeAggregate && !activeParticipantRealJid(key));
    }
    return activeParticipantNick(key) !== nick;
  });
  if (next.length === current.length) return;
  if (next.length === 0) {
    const all = { ...$mucActiveParticipants.get() };
    delete all[roomJid];
    $mucActiveParticipants.set(all);
  } else {
    $mucActiveParticipants.setKey(roomJid, next);
  }
  syncActiveParticipantsForRoom(roomJid);
}

/**
 * Apply an inbound presence update to the participants store. The
 * cases (XEP-0272 §Joining and §Leaving):
 * - Available presence + `muji.active === true` → add nick.
 * - Available presence + `muji.preparing === true` → setup signal
 *   only; preparing does not add OR clear active call membership,
 *   even when a separate resource is already active under the same
 *   nick.
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
    muc_jid?: string | null;
    muji?: { preparing: boolean; active: boolean };
  },
): void {
  if (!presence.from) return;
  const slash = presence.from.indexOf("/");
  if (slash < 0) return;
  const roomJid = normalizeMucCallRoomJid(presence.from.slice(0, slash));
  const nick = presence.from.slice(slash + 1);
  if (!roomJid || !nick) return;

  const isUnavailable = presence.presence_type === "unavailable";
  const hasPreparing = !isUnavailable && presence.muji?.preparing === true;
  const wantsActive = !isUnavailable && presence.muji?.active === true;
  const shouldClear =
    isUnavailable ||
    !presence.muji ||
    (presence.muji.preparing === false && presence.muji.active === false);

  // XEP-0272 §Joining: a presence containing `<preparing/>` echoed
  // back from the MUC is the signal a waiting client uses to proceed
  // to its content-declaring presence. Some conformant stanzas can
  // contain both `<preparing/>` and `<content/>`; preparing remains
  // the setup signal in that case. Fire any registered one-shot
  // listener BEFORE we touch the participant store so the
  // beginMucCall flow unblocks deterministically.
  if (hasPreparing) {
    addPreparingParticipant(roomJid, nick, presence.muc_jid);
    notifyPrepareEcho(roomJid, nick, presence.muc_jid);
  } else {
    removePreparingParticipant(roomJid, nick, presence.muc_jid, {
      includeAggregate: !presence.muc_jid,
    });
  }

  if (wantsActive) {
    addActiveParticipant(roomJid, nick, presence.muc_jid);
  } else if (shouldClear) {
    removeActiveParticipant(roomJid, nick, presence.muc_jid, {
      includeAggregate: !presence.muc_jid,
    });
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
  $mucActiveParticipants.set({});
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
  rejectPrepareEchoWaitersForParticipant(normalized, nick, err);
  rejectNoOtherPreparingWaitersForParticipant(normalized, nick, err);
}

function addPreparingParticipant(roomJid: string, nick: string, realJid?: string | null): void {
  const current = $mucPreparingParticipants.get()[roomJid] ?? [];
  const key = preparingParticipantKey(nick, realJid);
  if (current.includes(key)) return;
  $mucPreparingParticipants.setKey(roomJid, [...current, key]);
  notifyPreparingChanged(roomJid);
}

function removePreparingParticipant(
  roomJid: string,
  nick: string,
  realJid?: string | null,
  options: { includeAggregate?: boolean } = {},
): void {
  const current = $mucPreparingParticipants.get()[roomJid] ?? [];
  const normalizedRealJid = normalizeFullJid(realJid);
  const next = current.filter((key) => {
    if (preparingParticipantNick(key) !== nick) return true;
    if (normalizedRealJid) {
      if (samePreparingParticipant(key, nick, normalizedRealJid)) return false;
      return !(options.includeAggregate && !preparingParticipantRealJid(key));
    }
    return preparingParticipantNick(key) !== nick;
  });
  if (next.length === current.length) return;
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
 * echo. Keyed by `<room-jid>/<nick>/<real-jid>` when the room exposes
 * the occupant's real full JID, so same-account resources sharing a
 * MUC nick cannot consume each other's XEP-0272 preparing echo.
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
    selfFullJid?: string | null;
    resolve: () => void;
    reject: (err: Error) => void;
    timer: ReturnType<typeof setTimeout>;
  }
>();

function prepareEchoListenerKey(roomJid: string, nick: string, realJid?: string | null): string {
  return `${roomJid}/${preparingParticipantKey(nick, realJid)}`;
}

function notifyPrepareEcho(roomJid: string, nick: string, realJid?: string | null): void {
  const key = prepareEchoListenerKey(roomJid, nick, realJid);
  const listener = prepareEchoListeners.get(key);
  if (listener) {
    prepareEchoListeners.delete(key);
    clearTimeout(listener.timer);
    listener.resolve();
  }
}

function listenerMatchesParticipant(key: string, roomJid: string, nick: string): boolean {
  if (!key.startsWith(`${roomJid}/`)) return false;
  const participantKey = key.slice(roomJid.length + 1);
  return preparingParticipantNick(participantKey) === nick;
}

function rejectPrepareEchoWaitersForParticipant(roomJid: string, nick: string, err: Error): void {
  for (const key of [...prepareEchoListeners.keys()]) {
    if (!listenerMatchesParticipant(key, roomJid, nick)) continue;
    rejectPrepareEchoWaiter(key, err);
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

function isSelfPreparingParticipant(key: string, selfNick: string, selfFullJid?: string | null): boolean {
  if (preparingParticipantNick(key) !== selfNick) return false;
  const normalizedSelfFullJid = normalizeFullJid(selfFullJid);
  const realJid = preparingParticipantRealJid(key);
  if (normalizedSelfFullJid && realJid) return normalizedSelfFullJid === realJid;
  return !normalizedSelfFullJid && !realJid;
}

function hasOtherPreparing(roomJid: string, selfNick: string, selfFullJid?: string | null): boolean {
  return ($mucPreparingParticipants.get()[roomJid] ?? []).some((key) => !isSelfPreparingParticipant(key, selfNick, selfFullJid));
}

function notifyPreparingChanged(roomJid: string): void {
  for (const [key, listener] of noOtherPreparingListeners) {
    if (listener.roomJid !== roomJid) continue;
    if (hasOtherPreparing(listener.roomJid, listener.selfNick, listener.selfFullJid)) continue;
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

function rejectNoOtherPreparingWaitersForParticipant(roomJid: string, nick: string, err: Error): void {
  for (const key of [...noOtherPreparingListeners.keys()]) {
    if (!listenerMatchesParticipant(key, roomJid, nick)) continue;
    rejectNoOtherPreparingWaiter(key, err);
  }
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
  selfFullJid?: string | null,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const normalized = normalizeMucCallRoomJid(roomJid);
    const key = prepareEchoListenerKey(normalized, nick, selfFullJid);
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
  selfFullJid?: string | null,
): Promise<void> {
  const normalized = normalizeMucCallRoomJid(roomJid);
  if (!hasOtherPreparing(normalized, selfNick, selfFullJid)) {
    return Promise.resolve();
  }
  return new Promise((resolve, reject) => {
    const key = prepareEchoListenerKey(normalized, selfNick, selfFullJid);
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
      selfFullJid,
      resolve,
      reject,
      timer,
    });
  });
}
