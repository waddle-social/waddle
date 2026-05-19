import { atom } from "nanostores";
import { outboundCalls, type CallWireSender } from "./outbound";
import type { CallEvent, CallMedia, CallState, LiveKitJoin } from "./types";

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
          } else {
            // MUC group call: leave via the muc-call:0 IQ surface
            // AND clear our presence extension so other occupants
            // stop showing us as in-call. Both errors flow through
            // the outer try/catch + reportCallError so failures are
            // surfaced consistently with every other call-wire op.
            await sendMucCallLeave(sender, s.peer);
            const raw = sender as RawIqSender;
            if (s.selfNick && raw.update_muc_call_presence) {
              await raw.update_muc_call_presence(s.peer, s.selfNick, false, s.peer);
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
 */
export type RawIqSender = {
  send_muc_call_join?: (room_jid: string) => Promise<LiveKitJoin>;
  send_muc_call_leave?: (room_jid: string) => Promise<void>;
  update_muc_call_presence?: (
    room_jid: string,
    nick: string,
    active: boolean,
    call_id: string,
  ) => Promise<void>;
};

/**
 * Send `<request-join xmlns='urn:waddle:muc-call:0' room='...'/>`
 * via the typed wasm method and return the issued LiveKit join
 * credentials. The wasm side builds the IQ via `minidom::Element`
 * and parses the response into a typed `WaddleLiveKitJoin`, so no
 * XML touches the chat-side wire layer.
 */
async function sendMucCallJoin(
  sender: RawIqSender,
  roomJid: string,
): Promise<LiveKitJoin> {
  if (!sender.send_muc_call_join) {
    throw new Error(
      "wasm client does not expose send_muc_call_join; rebuild the wasm bundle",
    );
  }
  return await sender.send_muc_call_join(roomJid);
}

/**
 * Send `<request-leave/>` so the SFU unregisters us and revokes
 * any tokens it minted for this `(room, identity)` pair.
 */
export async function sendMucCallLeave(
  sender: RawIqSender | CallWireSender,
  roomJid: string,
): Promise<void> {
  const rawSender = sender as RawIqSender;
  if (!rawSender.send_muc_call_leave) {
    throw new Error(
      "wasm client does not expose send_muc_call_leave; rebuild the wasm bundle",
    );
  }
  await rawSender.send_muc_call_leave(roomJid);
}

/**
 * Originator action for MUC group calls: send `<request-join/>`,
 * pull the issued LiveKit join out of the IQ result, transition
 * `$callState` straight to `active` (no propose/proceed round-trip
 * — MUC calls are presence-discovered, so there's no responder to
 * ring).
 */
export async function beginMucCall(
  sender: RawIqSender,
  roomJid: string,
  media: CallMedia,
  selfNick?: string,
): Promise<void> {
  const join = await sendMucCallJoin(sender, roomJid);
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
  // Advertise our in-call status via MUC presence so other
  // occupants can show a "you have a live call in this channel"
  // indicator. Best-effort: if the wasm method is missing (stale
  // bundle), the call still works — just no in-band discovery.
  if (selfNick && sender.update_muc_call_presence) {
    try {
      await sender.update_muc_call_presence(roomJid, selfNick, true, roomJid);
    } catch (err) {
      reportCallError(err);
    }
  }
}
