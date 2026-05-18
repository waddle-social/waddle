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
 * the global store. Clears the last-error slot on a successful
 * state transition so old errors don't linger over a fresh call.
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
            // MUC group call: leave via the muc-call:0 IQ surface.
            // `peer` here is the room's bare JID (sid mirrors it).
            await sendMucCallLeave(sender, s.peer);
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
 * Type for `BrowserXmppClient.xmpp.send_raw_iq` — used by the MUC
 * call IQ surface, which is XML-based rather than typed (the
 * wasm bindings only expose typed methods for the JMI/Jingle 1:1
 * flow). Decoupled so unit tests can stub a partial client.
 */
export type RawIqSender = {
  send_raw_iq?: (xml: string) => Promise<string>;
};

const NS_WADDLE_MUC_CALL = "urn:waddle:muc-call:0";

function uuid(): string {
  return crypto.randomUUID();
}

/**
 * Send `<request-join xmlns='urn:waddle:muc-call:0' room='...'/>`
 * and parse the issued LiveKit transport out of the response.
 * Returns the join details ready to hand to `CallEngine.connect`.
 */
async function sendMucCallJoin(
  sender: RawIqSender,
  roomJid: string,
): Promise<LiveKitJoin> {
  if (!sender.send_raw_iq) {
    throw new Error("wasm client does not expose send_raw_iq; rebuild the wasm bundle");
  }
  const id = uuid();
  // Use double-quoted attribute values; the `room` attribute carries
  // a JID with no characters that need XML-escaping for our roomJid
  // shape (`local@domain`), but a paranoid escape would belong here
  // if we ever passed user-supplied strings.
  const xml = `<iq type="set" id="${id}" to="${roomJid}">`
    + `<request-join xmlns="${NS_WADDLE_MUC_CALL}" room="${roomJid}"/>`
    + `</iq>`;
  const resultXml = await sender.send_raw_iq(xml);
  return parseMucJoinResult(resultXml);
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
  if (!rawSender.send_raw_iq) {
    throw new Error("wasm client does not expose send_raw_iq; rebuild the wasm bundle");
  }
  const id = uuid();
  const xml = `<iq type="set" id="${id}" to="${roomJid}">`
    + `<request-leave xmlns="${NS_WADDLE_MUC_CALL}" room="${roomJid}"/>`
    + `</iq>`;
  await rawSender.send_raw_iq(xml);
}

/**
 * Parse the `<joined><transport xmlns="urn:waddle:transports:livekit:0">…</transport></joined>`
 * response into a typed `LiveKitJoin`. Throws on malformed input.
 *
 * Regex-based on purpose: keeps the chat layer free of an XML
 * parser dependency for a single well-known response shape, and
 * works identically across the browser, Bun's test runtime, and
 * Node-without-DOMParser.
 */
function parseMucJoinResult(xml: string): LiveKitJoin {
  const field = (name: string): string => {
    const re = new RegExp(`<${name}>([^<]*)</${name}>`);
    const m = xml.match(re);
    return m ? m[1] : "";
  };
  const url = field("url");
  const room = field("room");
  const identity = field("identity");
  const token = field("token");
  if (!url || !room || !identity || !token) {
    throw new Error("muc-call: response transport missing required fields");
  }
  return { url, room, identity, token };
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
): Promise<void> {
  const join = await sendMucCallJoin(sender, roomJid);
  $callState.set({
    phase: "active",
    peer: roomJid,
    sid: roomJid,
    media,
    join,
    kind: "muc",
  });
  $lastCallError.set(null);
}
