import { atom } from "nanostores";
import { outboundCalls, type CallWireSender } from "./outbound";
import type { CallEvent, CallMedia, CallState, LiveKitJoin } from "./types";
import { awaitPreparingEcho } from "./muc-call-presence";

/**
 * XEP-0272 §Joining MUST: a client emitting a preparing presence
 * MUST wait for the MUC to rebroadcast it before proceeding. The
 * MUC echo arrives over the same WebSocket session, so the wait
 * is bounded by the network round-trip. A 2s timeout covers
 * pathologically slow networks while preventing the call setup
 * from hanging indefinitely on a missed echo.
 */
const PREPARING_ECHO_TIMEOUT_MS = 2000;

/**
 * XEP-0166 §6.3 + XEP-0272 separate-IQ accept: the server's
 * session-accept arrives as an inbound `<iq type='set'>` after
 * the empty IQ-result ack of our session-initiate. We register
 * a resolver keyed by sid (= room JID per Waddle convention)
 * BEFORE sending the initiate, then await it after the initiate's
 * empty ack returns. 10s timeout covers slow SFU token mints.
 */
const MUJI_ACCEPT_TIMEOUT_MS = 10_000;

/**
 * Pending Muji session-accept resolvers keyed by sid (= room JID).
 * The `on_call` dispatcher routes an inbound
 * `CallEventKind::SessionAccept` whose sid matches a registered
 * entry to the resolver instead of running the 1:1 reducer — Muji
 * accepts are owned by `beginMucCall`'s Promise chain, not the
 * `$callState` reducer.
 */
const pendingMujiAccepts = new Map<
  string,
  { resolve: (join: LiveKitJoin) => void; reject: (err: Error) => void; timer: ReturnType<typeof setTimeout> }
>();

function registerPendingMujiAccept(sid: string): Promise<LiveKitJoin> {
  return new Promise<LiveKitJoin>((resolve, reject) => {
    const timer = setTimeout(() => {
      pendingMujiAccepts.delete(sid);
      reject(
        new Error(
          `Muji session-accept did not arrive within ${MUJI_ACCEPT_TIMEOUT_MS}ms`,
        ),
      );
    }, MUJI_ACCEPT_TIMEOUT_MS);
    pendingMujiAccepts.set(sid, { resolve, reject, timer });
  });
}

/**
 * Try to dispatch an inbound `session-accept` event to the pending
 * Muji-accept resolver, if any. Returns `true` when the event was
 * consumed (caller short-circuits the 1:1 reducer); `false` when
 * the event should fall through to normal handling.
 */
function tryFulfillMujiAccept(event: CallEvent): boolean {
  if (event.kind !== "session-accept") return false;
  const pending = pendingMujiAccepts.get(event.sid);
  if (!pending) return false;
  pendingMujiAccepts.delete(event.sid);
  clearTimeout(pending.timer);
  pending.resolve(event.join);
  return true;
}

/**
 * Single-slot call lifecycle store. Updated by `applyCallEvent`
 * whenever the WASM client's `on_call` callback fires; subscribed
 * to by the chat UI to render the ringing toast / in-call overlay.
 *
 * Tracks one call at a time. Concurrent calls would need a keyed
 * map; deferred until that's a real product requirement.
 */
export const $callState = atom<CallState>({ phase: "idle" });

/**
 * Surface for the most recent wire-send failure (`send_call_*`
 * rejected) so the call UI can render a small destructive-tinted
 * row instead of swallowing the error. Cleared on the next
 * successful state transition.
 */
export const $lastCallError = atom<string | null>(null);

/**
 * Pure reducer over the current call state. Exposed for unit
 * testing; the production caller is the `on_call` registration in
 * `lib/xmpp/client.ts`, which subscribes via `applyCallEvent`.
 *
 * Every session-* and JMI-ack event is guarded on the current
 * phase and a matching `sid`. A stale stanza (server replay,
 * delayed delivery, or a malicious peer) MUST NOT clobber a live
 * call slot, so unmatched events are dropped rather than
 * collapsing to `ended`.
 */
export function reduceCallState(current: CallState, event: CallEvent): CallState {
  switch (event.kind) {
    case "propose":
      // The remote peer is ringing us — surface as incoming so the
      // toast can offer accept/reject. If we're already in a call,
      // ignore; the side-effect handler emits `<reject/>` so the
      // proposer stops ringing.
      if (current.phase === "idle" || current.phase === "ended") {
        return {
          phase: "incoming",
          from: event.from,
          sid: event.sid,
          media: event.media,
        };
      }
      return current;
    case "proceed":
      // Our outbound propose was accepted; the next stanza we send
      // is session-initiate. The reducer keeps the call in
      // `outgoing`; `call-effects.ts` reads `proceed.from` and
      // emits the session-initiate stanza.
      return current;
    case "reject":
    case "retract":
      // XEP-0353 abort: only honor for the live call slot.
      if (
        current.phase !== "idle" &&
        "sid" in current &&
        current.sid === event.sid
      ) {
        return { phase: "ended", sid: event.sid, reason: event.kind };
      }
      return current;
    case "session-initiate":
      // Responder receives the Jingle session-initiate after they
      // accepted via JMI proceed. Only honor when we're the matching
      // incoming call.
      if (current.phase !== "incoming" || current.sid !== event.sid) {
        return current;
      }
      return {
        phase: "active",
        peer: event.from,
        sid: event.sid,
        media: event.media,
        join: event.join,
        kind: "dm",
      };
    case "session-accept":
      // Initiator receives the Jingle session-accept after the
      // server rewrote the transport with the initiator's own
      // LiveKit join token. Only honor when we're the matching
      // outgoing call.
      if (current.phase !== "outgoing" || current.sid !== event.sid) {
        return current;
      }
      return {
        phase: "active",
        peer: event.from,
        sid: event.sid,
        media: event.media,
        join: event.join,
        kind: "dm",
      };
    case "finish":
    case "session-terminate":
      // Either side hung up. Only end the live call slot — a stale
      // terminate for a different sid is a no-op.
      if (
        current.phase !== "idle" &&
        "sid" in current &&
        current.sid === event.sid
      ) {
        return {
          phase: "ended",
          sid: event.sid,
          reason: event.kind === "session-terminate" ? event.reason : null,
        };
      }
      return current;
  }
}

/**
 * Apply an inbound `CallEvent` from the WASM `on_call` callback to
 * the global store. Composes the pure `reduceCallState` with two
 * lifecycle side-effects (NOT pure — `reduceCallState` alone is
 * what unit tests exercise):
 *
 * 1. Clear `$lastCallError` on any non-trivial transition so an
 *    error from a prior call doesn't linger over a fresh one.
 * 2. Cancel the outgoing-ring auto-retract timer when leaving the
 *    `outgoing` phase. Done here, not in the reducer, because
 *    `setTimeout`/`clearTimeout` are global-state side-effects.
 */
export function applyCallEvent(event: CallEvent): void {
  // Route inbound Muji session-accept stanzas to the pending
  // `beginMucCall` Promise BEFORE the 1:1 reducer sees them. A
  // Muji accept has its sid equal to the room JID and arrives
  // from the SFU mixer (`calls.<domain>`); the 1:1 reducer would
  // mis-interpret it as a peer accepting a JMI ring. Consuming
  // here keeps the two flows cleanly separated.
  if (tryFulfillMujiAccept(event)) {
    return;
  }
  const before = $callState.get();
  const next = reduceCallState(before, event);
  if (next !== before) {
    $lastCallError.set(null);
    // Any transition out of outgoing cancels the auto-retract timer:
    // proceed→outgoing stays outgoing (timer keeps running), but the
    // moment session-accept lands or the call is rejected/retracted
    // the slot moves and the timer is moot.
    if (before.phase === "outgoing" && next.phase !== "outgoing") {
      cancelOutgoingTimeout();
    }
  }
  $callState.set(next);
}

/**
 * Mark the local call slot as idle. Called when the UI dismisses
 * the `ended` state (the toast closes, the user clicks dismiss).
 */
export function clearCallState(): void {
  cancelOutgoingTimeout();
  $callState.set({ phase: "idle" });
  $lastCallError.set(null);
}

/**
 * Originator-side transition: snapshot the outbound `<propose/>`
 * intent into the store so the UI can render the outgoing-ring
 * overlay. The wire send is the caller's responsibility — keep
 * the store side-effect free for clean unit testing.
 */
export function beginOutgoingCall(to: string, sid: string, media: CallMedia): void {
  $callState.set({ phase: "outgoing", to, sid, media });
  $lastCallError.set(null);
}

/**
 * Outgoing-call ring timeout in milliseconds. After this elapses
 * with the slot still in `phase: outgoing` against the same sid,
 * the call is auto-retracted so the toast doesn't ring forever
 * when the responder is offline / has notifications muted.
 */
const OUTGOING_TIMEOUT_MS = 45_000;

let outgoingTimer: ReturnType<typeof setTimeout> | null = null;

/**
 * Schedule the auto-retract for the most recent `beginOutgoingCall`.
 * The timer fires `outboundCalls.retract` if the slot is still in
 * `outgoing` with the supplied `sid`; otherwise the timeout is a
 * no-op. Any subsequent state transition cancels the previous timer
 * (a fresh `beginOutgoingCall` replaces it, and `clearCallState` /
 * `tearDownActiveCall` cancel it directly).
 *
 * Sender-supplied so the store stays oblivious to the wasm client —
 * `CallButton.startCall` passes the same `sender` it used for the
 * propose.
 */
export function scheduleOutgoingTimeout(
  sender: CallWireSender,
  sid: string,
  timeoutMs: number = OUTGOING_TIMEOUT_MS,
): void {
  cancelOutgoingTimeout();
  outgoingTimer = setTimeout(() => {
    outgoingTimer = null;
    const s = $callState.get();
    if (s.phase !== "outgoing" || s.sid !== sid) return;
    void outboundCalls
      .retract(sender, s.to, s.sid)
      .catch((err) => reportCallError(err));
    $callState.set({
      phase: "ended",
      sid,
      reason: "timeout",
    });
  }, timeoutMs);
}

function cancelOutgoingTimeout(): void {
  if (outgoingTimer) {
    clearTimeout(outgoingTimer);
    outgoingTimer = null;
  }
}

/**
 * Surface a wire-send failure into the global error slot. Components
 * subscribe via `$lastCallError` to render an inline error row.
 * Auto-clears after `AUTO_CLEAR_MS` so the row doesn't linger past
 * the user's next action.
 */
const AUTO_CLEAR_MS = 8000;
let clearTimer: ReturnType<typeof setTimeout> | null = null;

export function reportCallError(err: unknown): void {
  const message = err instanceof Error ? err.message : String(err);
  $lastCallError.set(message);
  if (clearTimer) clearTimeout(clearTimer);
  clearTimer = setTimeout(() => $lastCallError.set(null), AUTO_CLEAR_MS);
}

/**
 * Drop the active error without touching `$callState`. Used by the
 * global error toast's dismiss button — `clearCallState` is the
 * wrong tool when phase is already `idle` or `ended`, since it
 * would still emit a redundant state set.
 */
export function clearLastCallError(): void {
  if (clearTimer) {
    clearTimeout(clearTimer);
    clearTimer = null;
  }
  $lastCallError.set(null);
}

/**
 * Best-effort teardown for the live call slot before the XMPP
 * connection drops or the call surface unmounts. Sends the
 * appropriate XEP-0166 / XEP-0353 stanza to the peer (so they don't
 * end up stuck on a dangling call), then clears the slot.
 *
 * `reason` distinguishes graceful hangup ("success") from
 * unreachable-side teardown ("gone", per XEP-0166 §7.4) so the peer
 * UI can label the ended state correctly.
 */
export async function tearDownActiveCall(
  sender: CallWireSender | null,
  reason: "success" | "gone",
): Promise<void> {
  cancelOutgoingTimeout();
  const s = $callState.get();
  if (sender) {
    try {
      switch (s.phase) {
        case "active":
          if (s.kind === "dm") {
            await outboundCalls.sessionTerminate(sender, s.peer, s.sid, reason);
            // XEP-0353 §3.5: both parties SHOULD send `<finish/>` after
            // the call ends so MAM archives both sides consistently
            // (the JMI message stack uses the finish marker to bookend
            // the call record). Best-effort: a wasm bundle that
            // doesn't expose `send_call_finish` shouldn't keep the
            // terminate from clearing the local slot.
            try {
              await outboundCalls.finish(sender, s.peer, s.sid);
            } catch (err) {
              reportCallError(err);
            }
          } else {
            // MUC group call: clear our Muji presence AND leave via
            // the IQ surface. Presence-first ordering (XEP-0272
            // §Leaving: "Updating the presence first reduces the
            // likelihood of situations where new participants
            // initiate sessions with participants who are leaving").
            // Other occupants drive the "N in call" indicator off
            // our `<muji/>` advertisement, so dropping it is what
            // makes the call visibly stop. Each side gets its own
            // try/catch so a single failure (e.g. a stale wasm
            // bundle) can't swallow the other.
            const raw = sender as RawIqSender;
            if (s.selfNick && raw.update_muji_presence) {
              try {
                await raw.update_muji_presence(
                  s.peer,
                  s.selfNick,
                  false, // active
                  false, // preparing
                  false, // video
                );
              } catch (err) {
                reportCallError(err);
              }
            }
            try {
              await sendMujiSessionTerminate(raw, s.peer);
            } catch (err) {
              reportCallError(err);
            }
          }
          break;
        case "outgoing":
          await outboundCalls.retract(sender, s.to, s.sid);
          break;
        case "incoming":
          await outboundCalls.reject(sender, s.from, s.sid);
          break;
        default:
          break;
      }
    } catch (err) {
      reportCallError(err);
    }
  }
  $callState.set({ phase: "idle" });
}

/**
 * Subset of the wasm `WaddleClient` API used by the MUC group-call
 * surface. Typed methods only — no `send_raw_iq` string-concat
 * escape hatch (CLAUDE.md XML-generation rule). Optional fields so
 * unit tests can stub a partial client.
 *
 * Fully XEP-conformant: the Muji presence advertisement
 * (`update_muji_presence`) carries XEP-0272 `<muji/>` in MUC
 * presence, and `send_muji_session_initiate` /
 * `send_muji_session_terminate` issue XEP-0166 Jingle
 * session-initiate / session-terminate stanzas to the SFU mixer
 * JID (`calls.<server-domain>`) with a `<muji room='…'/>` payload
 * — the wire shape every other conformant client expects.
 */
export type RawIqSender = {
  /**
   * Send the XEP-0272 Muji-bearing Jingle `session-initiate` IQ
   * to the SFU mixer (`calls.<server-domain>`). Resolves when the
   * server's EMPTY IQ-result ack arrives — the actual
   * session-accept (and its LiveKit credentials) is delivered as
   * a SEPARATE server-initiated `<iq type='set'>` per XEP-0166
   * §6.3 and surfaced through the `on_call` callback. The caller
   * (see `sendMujiSessionInitiate` below) registers a pending
   * resolver before invoking this method.
   */
  send_muji_session_initiate?: (
    room_jid: string,
    video: boolean,
  ) => Promise<void>;
  send_muji_session_terminate?: (room_jid: string) => Promise<void>;
  update_muji_presence?: (
    room_jid: string,
    nick: string,
    active: boolean,
    preparing: boolean,
    video: boolean,
  ) => Promise<void>;
};

/**
 * XEP-0166 §6.3 + XEP-0272 separate-IQ accept flow:
 *
 *   1. Register a pending Muji-accept resolver keyed by `roomJid`
 *      BEFORE sending so an inbound session-accept can't beat us
 *      to the table.
 *   2. Send the session-initiate IQ via the wasm bridge. The
 *      Promise it returns resolves on the empty IQ-result ack.
 *   3. Await the pending resolver — fulfilled when `applyCallEvent`
 *      routes the inbound `session-accept` to us via
 *      `tryFulfillMujiAccept`.
 */
async function sendMujiSessionInitiate(
  sender: RawIqSender,
  roomJid: string,
  video: boolean,
): Promise<LiveKitJoin> {
  if (!sender.send_muji_session_initiate) {
    throw new Error(
      "wasm client does not expose send_muji_session_initiate; rebuild the wasm bundle",
    );
  }
  const acceptWait = registerPendingMujiAccept(roomJid);
  try {
    await sender.send_muji_session_initiate(roomJid, video);
  } catch (err) {
    // Roll back the pending registration so a future call attempt
    // doesn't resolve against this aborted one.
    const pending = pendingMujiAccepts.get(roomJid);
    if (pending) {
      pendingMujiAccepts.delete(roomJid);
      clearTimeout(pending.timer);
    }
    throw err;
  }
  return await acceptWait;
}

/**
 * Send a XEP-0272 Muji-bearing Jingle `session-terminate` so the
 * SFU unregisters us and revokes any tokens it minted for this
 * `(room, identity)` pair.
 */
async function sendMujiSessionTerminate(
  sender: RawIqSender | CallWireSender,
  roomJid: string,
): Promise<void> {
  const rawSender = sender as RawIqSender;
  if (!rawSender.send_muji_session_terminate) {
    throw new Error(
      "wasm client does not expose send_muji_session_terminate; rebuild the wasm bundle",
    );
  }
  await rawSender.send_muji_session_terminate(roomJid);
}

/**
 * Originator action for MUC group calls. Implements XEP-0272
 * §Joining's two-phase flow:
 *
 *   1. Emit `<muji><preparing/></muji>` MUC presence so other
 *      occupants know we are about to join (the XEP MUSTs this).
 *   2. Send the Jingle session-initiate to the SFU mixer.
 *   3. Once the session-accept arrives with a LiveKit token,
 *      re-emit `<muji>` with `<content/>` children so we move
 *      from "preparing" to "in call" on the wire.
 *   4. Transition `$callState` to `active`.
 *
 * XEP-0272's MUST about waiting for the MUC rebroadcast of the
 * preparing presence is satisfied implicitly: the session-initiate
 * IQ round-trip takes longer than the in-process presence echo
 * back from the local MUC, so by the time we move to step 3 the
 * room has already reflected step 1.
 *
 * No propose/proceed JMI round-trip — MUC calls are
 * presence-discovered, so there's no responder to ring.
 */
export async function beginMucCall(
  sender: RawIqSender,
  roomJid: string,
  media: CallMedia,
  selfNick?: string,
): Promise<void> {
  // Step 1 — preparing presence (XEP-0272 §Joining two-phase flow).
  // Register the echo waiter BEFORE emitting so a fast MUC echo
  // can't fire before our listener is ready. The await below
  // blocks until the MUC rebroadcasts our preparing presence (or
  // the 2s safety timeout fires). Best-effort: a stale wasm
  // bundle without `update_muji_presence` skips the two-phase
  // shape entirely; the call still works but isn't strict-XEP.
  if (selfNick && sender.update_muji_presence) {
    const echoWait = awaitPreparingEcho(
      roomJid,
      selfNick,
      PREPARING_ECHO_TIMEOUT_MS,
    );
    try {
      await sender.update_muji_presence(
        roomJid,
        selfNick,
        false, // active (no <content/> yet)
        true, // preparing
        false, // video — irrelevant in preparing phase
      );
    } catch (err) {
      reportCallError(err);
    }
    // XEP-0272 §Joining: "The client MUST then wait until the MUC
    // rebroadcasts its presence message". We satisfy the literal
    // MUST by holding here for the echo.
    await echoWait;
  }

  // Step 2 — session-initiate. By now the MUC has acknowledged
  // our preparing presence (per XEP-0272 §Joining MUST), so the
  // SFU mixer's session-accept arrives in a deterministic order.
  const join = await sendMujiSessionInitiate(sender, roomJid, media.video);
  $callState.set({
    phase: "active",
    peer: roomJid,
    sid: roomJid,
    media,
    join,
    kind: "muc",
    selfNick,
  });
  $lastCallError.set(null);

  // Step 3 — content-declaring presence. Other occupants now see
  // the chip pulse because `<muji>` carries `<content/>` (our
  // store helper's `is_active` check). Audio is implied; we
  // advertise video only when the user enabled it.
  if (selfNick && sender.update_muji_presence) {
    try {
      await sender.update_muji_presence(
        roomJid,
        selfNick,
        true, // active
        false, // preparing
        media.video, // video
      );
    } catch (err) {
      reportCallError(err);
    }
  }
}
