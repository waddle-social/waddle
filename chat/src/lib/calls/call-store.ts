import { atom } from "nanostores";
import { outboundCalls, type CallWireSender } from "./outbound";
import type { CallEvent, CallMedia, CallState, LiveKitJoin } from "./types";
import {
  awaitPreparingEcho,
  awaitNoOtherPreparing,
  cancelMucCallPreparationWaiters,
  clearMucCallParticipant,
  normalizeMucCallRoomJid,
} from "./muc-call-presence";
import {
  applyDmCallEvent,
  clearDmCallActivity,
} from "./dm-call-activity";
import {
  forgetMucCallSession,
  rememberMucCallSession,
} from "./muc-call-session-cache";
import { clearLiveCallParticipants } from "./muc-call-live-participants";
import { selfRaisedHandFor, setSelfRaisedHand } from "./call-raised-hand";
import { mediaErrorMessage } from "./call-media-issues";
import {
  beginCallAttempt,
  finishCallAttempt,
  markCallAttemptAccepted,
  reportFailedCallAttempt,
  type CallEndReason,
  type CallSetupOutcome,
} from "./call-lifecycle-telemetry";
import {
  dmCallStartedAnchorFromTransition,
  publishDmCallOutcomeAnchor,
  publishDmCallStartedAnchor,
} from "./dm-call-anchor";
import { barePeerJid } from "../xmpp/jid";
import type { IncomingCallAlertController } from "@/shell/audio-alerts";
import { reportError as reportTelemetryError } from "../telemetry";

/**
 * XEP-0272 §Joining MUST: a client emitting a preparing presence
 * MUST wait for the MUC to rebroadcast it before proceeding. The
 * MUC echo arrives over the same WebSocket session, so the wait
 * is bounded by the network round-trip. A 2s timeout covers
 * pathologically slow networks while preventing the call setup
 * from hanging indefinitely on a missed echo.
 */
const PREPARING_ECHO_TIMEOUT_MS = 2000;
const PREPARING_PEERS_TIMEOUT_MS = 2000;

/**
 * XEP-0166 §6.3 + XEP-0272 separate-IQ accept: the server's
 * session-accept arrives as an inbound `<iq type='set'>` after
 * the empty IQ-result ack of our session-initiate. We register
 * a resolver keyed by the per-attempt Jingle sid before sending,
 * BEFORE sending the initiate, then await it after the initiate's
 * empty ack returns. 10s timeout covers slow SFU token mints.
 */
const MUJI_ACCEPT_TIMEOUT_MS = 10_000;

/**
 * Pending Muji session-accept resolvers keyed by per-attempt Jingle sid.
 * The `on_call` dispatcher routes an inbound
 * `CallEventKind::SessionAccept` whose sid matches a registered
 * entry to the resolver instead of running the 1:1 reducer — Muji
 * accepts are owned by `beginMucCall`'s Promise chain, not the
 * `$callState` reducer.
 */
type PendingMujiAccept = {
  roomJid: string;
  attemptId: string;
  expectedFrom?: string;
  resolve: (join: LiveKitJoin) => void;
  reject: (err: Error) => void;
  timer: ReturnType<typeof setTimeout>;
};

const pendingMujiAccepts = new Map<string, PendingMujiAccept>();

function rejectPendingMujiAccept(
  sid: string,
  err: Error,
  attemptId?: string,
): void {
  const pending = pendingMujiAccepts.get(sid);
  if (!pending) return;
  if (attemptId && pending.attemptId !== attemptId) return;
  pendingMujiAccepts.delete(sid);
  clearTimeout(pending.timer);
  pending.reject(err);
}

function rejectAllPendingMujiAccepts(err: Error): void {
  for (const [sid, pending] of pendingMujiAccepts) {
    pendingMujiAccepts.delete(sid);
    clearTimeout(pending.timer);
    pending.reject(err);
  }
}

function normalizeExpectedMujiAcceptFrom(jid?: string | null): string | undefined {
  const bare = barePeerJid(jid ?? "").toLowerCase();
  return bare || undefined;
}

function registerPendingMujiAccept(
  sid: string,
  roomJid: string,
  attemptId: string,
  expectedFrom?: string | null,
): Promise<LiveKitJoin> {
  return new Promise<LiveKitJoin>((resolve, reject) => {
    const previous = pendingMujiAccepts.get(sid);
    if (previous) {
      pendingMujiAccepts.delete(sid);
      clearTimeout(previous.timer);
      previous.reject(
        new Error("Muji session-accept wait replaced by a new call attempt"),
      );
    }
    let entry: PendingMujiAccept;
    const timer = setTimeout(() => {
      if (pendingMujiAccepts.get(sid) === entry) {
        pendingMujiAccepts.delete(sid);
      }
      reject(
        new Error(
          `Muji session-accept did not arrive within ${MUJI_ACCEPT_TIMEOUT_MS}ms`,
        ),
      );
    }, MUJI_ACCEPT_TIMEOUT_MS);
    entry = {
      roomJid,
      attemptId,
      expectedFrom: normalizeExpectedMujiAcceptFrom(expectedFrom),
      resolve,
      reject,
      timer,
    };
    pendingMujiAccepts.set(sid, entry);
  });
}

function isExpectedMujiAccept(event: CallEvent, pending: {
  roomJid: string;
  expectedFrom?: string;
}): boolean {
  if (event.kind !== "session-accept") return false;
  const eventRoom = normalizeMucCallRoomJid(event.join.room);
  if (eventRoom !== pending.roomJid) return false;
  if (!pending.expectedFrom) return true;
  return barePeerJid(event.from).toLowerCase() === pending.expectedFrom;
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
  if (!isExpectedMujiAccept(event, pending)) return true;
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

let incomingCallAlerts: IncomingCallAlertController | null = null;

export function configureIncomingCallAlerts(
  controller: IncomingCallAlertController | null,
): void {
  incomingCallAlerts?.stopAll();
  incomingCallAlerts = controller;
}

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
    case "ringing":
      if (
        current.phase === "outgoing" &&
        current.sid === event.sid &&
        barePeerJid(event.from).toLowerCase() === barePeerJid(current.to).toLowerCase()
      ) {
        return { ...current, ringing: true };
      }
      return current;
    case "proceed":
      // Our outbound propose was accepted; the next stanza we send
      // is session-initiate. The reducer keeps the call in
      // `outgoing`; `call-effects.ts` reads `proceed.from` and
      // emits the session-initiate stanza.
      if (current.phase === "incoming" && current.sid === event.sid) {
        return { phase: "idle" };
      }
      return current;
    case "reject":
    case "retract":
      // XEP-0353 abort: only honor for the live call slot.
      if (
        current.phase !== "idle" &&
        "sid" in current &&
        current.sid === event.sid
      ) {
        return {
          phase: "ended",
          sid: event.sid,
          reason: event.tieBreak && event.reason === "expired"
            ? "expired"
            : event.kind,
        };
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
        initiator: event.from,
      };
    case "session-accept":
      // Initiator receives the Jingle session-accept after the
      // server rewrote the transport with the initiator's own
      // LiveKit join token. Only honor when we're the matching
      // outgoing call.
      if (current.phase !== "outgoing" || current.sid !== event.sid) {
        return current;
      }
      return current.initiator ? {
        phase: "active",
        peer: event.from,
        sid: event.sid,
        media: event.media,
        join: event.join,
        kind: "dm",
        initiator: current.initiator,
      } : {
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
          reason: event.reason ?? null,
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
 * 2. Cancel the outgoing-ring auto-retract timer when the peer
 *    proceeds or when leaving the `outgoing` phase. Done here, not
 *    in the reducer, because
 *    `setTimeout`/`clearTimeout` are global-state side-effects.
 */
export function applyCallEvent(
  event: CallEvent,
  options: { sender?: CallWireSender | null } = {},
): void {
  // Route inbound Muji session-accept stanzas to the pending
  // `beginMucCall` Promise BEFORE the 1:1 reducer sees them. A
  // Muji accept carries a per-attempt sid and arrives from the SFU
  // mixer (`calls.<domain>`). The 1:1 reducer would mis-interpret
  // it as a peer accepting a JMI ring. Consuming here keeps the two
  // flows cleanly separated.
  if (tryFulfillMujiAccept(event)) {
    return;
  }
  const before = $callState.get();
  const next = reduceCallState(before, event);
  if (
    next.phase === "incoming"
    && (before.phase !== "incoming" || before.sid !== next.sid)
  ) {
    beginCallAttempt(next.sid, "dm");
  }
  if (next.phase === "active" && (before.phase !== "active" || before.sid !== next.sid)) {
    markCallAttemptAccepted(next.sid);
  }
  if (next.phase === "ended" && (before.phase !== "ended" || before.sid !== next.sid)) {
    if (event.kind === "reject") {
      finishCallAttempt(next.sid, { setupOutcome: "declined" });
    } else if (event.kind === "retract") {
      finishCallAttempt(next.sid, { setupOutcome: "proposed" });
    } else {
      finishCallAttempt(next.sid, callLifecycleTerminalForRemoteEnd(before, event, next.reason));
    }
  }
  if (next !== before) {
    $lastCallError.set(null);
    // Any transition out of outgoing cancels the auto-retract timer.
    if (before.phase === "outgoing" && next.phase !== "outgoing") {
      cancelCallTimers();
    }
  }
  if (
    before.phase === "outgoing" &&
    event.kind === "proceed" &&
    event.sid === before.sid
  ) {
    cancelOutgoingTimeout();
  }
  $callState.set(next);
  applyIncomingCallAlerts(before, next);
  emitRingingOnIncomingPropose(before, next, event, options.sender ?? null);
  // Surface the DM "started a call" card live (the server only enriches the
  // archived `<proceed/>` row, which never reaches the live timeline).
  const dmCallAnchor = dmCallStartedAnchorFromTransition(before, next, new Date().toISOString());
  if (dmCallAnchor) publishDmCallStartedAnchor(dmCallAnchor);
}

const CALL_ERROR_REASONS = new Set([
  "connectivity-error",
  "failed-application",
  "failed-transport",
  "general-error",
  "incompatible-parameters",
  "media-error",
  "security-error",
  "unsupported-applications",
  "unsupported-transports",
  "error",
]);

export function callLifecycleTerminalForRemoteEnd(
  before: CallState,
  _event: CallEvent,
  reason: string | null,
): { setupOutcome?: CallSetupOutcome; endReason?: CallEndReason } {
  const isFailure = reason !== null && CALL_ERROR_REASONS.has(reason);
  if (before.phase === "active") {
    return { endReason: isFailure || reason === "timeout" ? "error" : "peer-left" };
  }
  if (reason === "timeout") return { setupOutcome: "timeout" };
  if (reason === "decline") return { setupOutcome: "declined" };
  if (isFailure) return { setupOutcome: "failed" };
  return { setupOutcome: "proposed" };
}

function applyIncomingCallAlerts(before: CallState, next: CallState): void {
  if (before.phase === "incoming" && (next.phase !== "incoming" || next.sid !== before.sid)) {
    incomingCallAlerts?.stop(before.sid);
  }
  if (next.phase === "incoming" && (before.phase !== "incoming" || before.sid !== next.sid)) {
    incomingCallAlerts?.start({
      peerJid: barePeerJid(next.from) || next.from,
      sid: next.sid,
      media: next.media,
    });
  }
}

function emitRingingOnIncomingPropose(
  before: CallState,
  next: CallState,
  event: CallEvent,
  sender: CallWireSender | null,
): void {
  if (!sender || event.kind !== "propose") return;
  if (next.phase !== "incoming" || next.sid !== event.sid) return;
  if (before.phase === "incoming" && before.sid === event.sid) return;
  void outboundCalls
    .ringing(sender, barePeerJid(event.from) || event.from, event.sid)
    .catch((err) => reportCallError(err));
}

/**
 * Mark the local call slot as idle. Called when the UI dismisses
 * the `ended` state (the toast closes, the user clicks dismiss).
 */
export function clearCallState(terminal: {
  setupOutcome?: CallSetupOutcome;
  endReason?: CallEndReason;
} = {}): void {
  cancelCallTimers();
  rejectAllPendingMujiAccepts(
    new Error("Muji session-accept wait cancelled while clearing call state"),
  );
  const current = $callState.get();
  if (current.phase !== "idle" && current.phase !== "ended") {
    finishCallAttempt(current.sid, {
      setupOutcome: terminal.setupOutcome ?? teardownSetupOutcome(current.phase),
      ...(terminal.endReason
        ? { endReason: terminal.endReason }
        : current.phase === "active" ? { endReason: "error" as const } : {}),
    });
  }
  if (current.phase === "incoming") {
    incomingCallAlerts?.stop(current.sid);
  } else {
    incomingCallAlerts?.stopAll();
  }
  if (current.phase === "muc-pending") {
    cancelMucCallPreparationWaiters(
      current.peer,
      current.selfNick,
      new Error("Muji preparing wait cancelled while clearing call state"),
    );
    clearLiveCallParticipants(current.peer);
  } else if (current.phase === "active" && current.kind === "muc") {
    clearLiveCallParticipants(current.peer);
  }
  $callState.set({ phase: "idle" });
  $lastCallError.set(null);
}

export function failCallState(err: unknown, sid: string): void {
  cancelCallTimers();
  finishCallAttempt(sid, { setupOutcome: "failed", endReason: "error" });
  $callState.set({ phase: "ended", sid, reason: "error" });
  reportCallError(err);
}

export function acceptIncomingTieBreakPropose(
  event: Extract<CallEvent, { kind: "propose" }>,
): void {
  cancelCallTimers();
  const replaced = $callState.get();
  if (replaced.phase === "active" && replaced.sid !== event.sid) {
    finishCallAttempt(replaced.sid, {
      setupOutcome: "accepted",
      endReason: "peer-left",
    });
  }
  $callState.set({
    phase: "incoming",
    from: event.from,
    sid: event.sid,
    media: event.media,
  });
  beginCallAttempt(event.sid, "dm");
  incomingCallAlerts?.start({
    peerJid: barePeerJid(event.from) || event.from,
    sid: event.sid,
    media: event.media,
  });
  $lastCallError.set(null);
}

/**
 * Originator-side transition: snapshot the outbound `<propose/>`
 * intent into the store so the UI can render the outgoing-ring
 * overlay. The wire send is the caller's responsibility — keep
 * the store side-effect free for clean unit testing.
 */
export function beginOutgoingCall(
  to: string,
  sid: string,
  media: CallMedia,
  initiator?: string,
): void {
  cancelCallTimers();
  $callState.set(
    initiator
      ? { phase: "outgoing", to, sid, media, initiator }
      : { phase: "outgoing", to, sid, media },
  );
  beginCallAttempt(sid, "dm");
  applyDmCallEvent({
    event: {
      kind: "propose",
      from: initiator ?? "",
      sid,
      media,
    },
    selfBareJid: initiator ?? "",
    selfFullJid: initiator,
    to,
    directionHint: "outgoing",
  });
  $lastCallError.set(null);
}

/**
 * Outgoing-call ring timeout in milliseconds. After this elapses
 * with the slot still in `phase: outgoing` against the same sid,
 * the call is auto-retracted so the toast doesn't ring forever
 * when the responder is offline / has notifications muted.
 */
const OUTGOING_TIMEOUT_MS = 45_000;
const SESSION_ACCEPT_TIMEOUT_MS = 45_000;

let outgoingTimer: ReturnType<typeof setTimeout> | null = null;
let sessionAcceptTimer: ReturnType<typeof setTimeout> | null = null;
let sessionAcceptTimeoutMs = SESSION_ACCEPT_TIMEOUT_MS;

function publishNoAnswerOutcome(peerJid: string, sid: string, media: CallMedia, initiator?: string): void {
  publishDmCallOutcomeAnchor({
    peerBareJid: barePeerJid(peerJid),
    sid,
    media,
    outcome: "no-answer",
    ...(initiator ? { initiator } : {}),
    ended: new Date().toISOString(),
  });
}

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
    publishNoAnswerOutcome(s.to, sid, s.media, s.initiator);
    clearDmCallActivity(s.to, sid);
    finishCallAttempt(sid, { setupOutcome: "timeout" });
    $callState.set({
      phase: "ended",
      sid,
      reason: "timeout",
    });
  }, timeoutMs);
}

/**
 * After a responder sends XEP-0353 `<proceed/>`, the initiator's
 * ringing timeout is cancelled because the peer did answer. The
 * call is still not active, though, until XEP-0166
 * `<session-accept/>` arrives. This second timeout covers the gap
 * where session-initiate was sent and acked, but the accept never
 * comes back.
 */
export function scheduleSessionAcceptTimeout(
  sender: CallWireSender,
  peerFullJid: string,
  sid: string,
  timeoutMs: number = sessionAcceptTimeoutMs,
): void {
  cancelSessionAcceptTimeout();
  sessionAcceptTimer = setTimeout(() => {
    sessionAcceptTimer = null;
    const s = $callState.get();
    if (s.phase !== "outgoing" || s.sid !== sid) return;
    void outboundCalls
      .sessionTerminate(sender, peerFullJid, sid, "timeout")
      .catch((err) => reportCallError(err));
    publishNoAnswerOutcome(peerFullJid, sid, s.media, s.initiator);
    clearDmCallActivity(peerFullJid, sid);
    finishCallAttempt(sid, { setupOutcome: "timeout" });
    $callState.set({
      phase: "ended",
      sid,
      reason: "timeout",
    });
  }, timeoutMs);
}

export function setSessionAcceptTimeoutMsForTests(timeoutMs: number): () => void {
  const previous = sessionAcceptTimeoutMs;
  sessionAcceptTimeoutMs = timeoutMs;
  return () => {
    sessionAcceptTimeoutMs = previous;
  };
}

function cancelOutgoingTimeout(): void {
  if (outgoingTimer) {
    clearTimeout(outgoingTimer);
    outgoingTimer = null;
  }
}

function cancelSessionAcceptTimeout(): void {
  if (sessionAcceptTimer) {
    clearTimeout(sessionAcceptTimer);
    sessionAcceptTimer = null;
  }
}

function cancelCallTimers(): void {
  cancelOutgoingTimeout();
  cancelSessionAcceptTimeout();
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
  // Map known camera/mic capture failures to friendly copy; fall back
  // to the raw message for everything else. Routine device-less /
  // permission-denied joins surface via the non-blocking media notice,
  // not here — this mapping only matters if a recognizable media
  // DOMException reaches the generic error surface through some other
  // path (e.g. a mid-call device switch in the settings dialog).
  const message =
    mediaErrorMessage(err) ?? (err instanceof Error ? err.message : String(err));
  reportTelemetryError("call.operation", err, {
    recoverable: true,
    detail: "call-operation",
  });
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
/**
 * Setup outcome for a call attempt that is being torn down / cleared
 * from a given phase. Shared by `clearCallState` and
 * `tearDownActiveCall` so the two teardown paths cannot drift: a
 * `muc-pending` group-call setup that gets torn down is a *failed*
 * attempt (its Muji accept/preparation waiters are cancelled), never
 * a merely "proposed" one.
 */
export function teardownSetupOutcome(phase: CallState["phase"]): CallSetupOutcome {
  switch (phase) {
    case "active":
      return "accepted";
    case "incoming":
      return "declined";
    case "muc-pending":
      return "failed";
    default:
      return "proposed";
  }
}

export async function tearDownActiveCall(
  sender: CallWireSender | null,
  reason: "success" | "gone",
): Promise<void> {
  cancelCallTimers();
  const s = $callState.get();
  if (s.phase !== "idle" && s.phase !== "ended") {
    finishCallAttempt(s.sid, {
      setupOutcome: teardownSetupOutcome(s.phase),
      ...(s.phase === "active"
        ? { endReason: reason === "success" ? "hangup" as const : "error" as const }
        : {}),
    });
  }
  if (sender) {
    try {
      switch (s.phase) {
        case "active":
          if (s.kind === "dm") {
            let terminateAllowsFinish = false;
            try {
              const outcome = await outboundCalls.sessionTerminateWithOutcome(
                sender,
                s.peer,
                s.sid,
                reason,
              );
              terminateAllowsFinish = outcome === "ok" || outcome === "orphaned";
              if (outcome === "error") {
                reportCallError(new Error("call session terminate failed"));
              }
            } catch (err) {
              reportCallError(err);
            }
            if (terminateAllowsFinish) {
              try {
                // XEP-0353 §3.5: both parties SHOULD send `<finish/>`
                // after the call ends so MAM archives both sides
                // consistently. A classified orphaned Jingle terminate
                // means the server has already lost the call registry, so
                // only the message-level bookend can still route.
                await outboundCalls.finish(sender, s.peer, s.sid);
              } catch (err) {
                reportCallError(err);
              }
            }
            clearDmCallActivity(s.peer, s.sid);
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
                await raw.update_muji_presence(s.peer, s.selfNick, false, false, false, {
                  handRaised: false,
                  muted: false, // leaving clears all in-call state
                });
              } catch (err) {
                reportCallError(err);
              } finally {
                clearMucCallParticipant(s.peer, s.selfNick, s.selfFullJid);
                setSelfRaisedHand(s.peer, false);
              }
            }
            clearLiveCallParticipants(s.peer);
            try {
              await sendMujiSessionTerminate(raw, s.peer, s.sid);
              forgetMucCallSession({
                roomJid: s.peer,
                selfFullJid: s.selfFullJid,
                sid: s.sid,
              });
            } catch (err) {
              reportCallError(err);
            }
          }
          break;
        case "outgoing":
          try {
            await outboundCalls.retract(sender, s.to, s.sid);
          } finally {
            clearDmCallActivity(s.to, s.sid);
          }
          break;
        case "incoming":
          try {
            await outboundCalls.reject(sender, s.from, s.sid);
          } finally {
            incomingCallAlerts?.stop(s.sid);
            clearDmCallActivity(s.from, s.sid);
          }
          break;
        case "muc-pending":
          rejectPendingMujiAccept(
            s.sid,
            new Error("Muji session-accept wait cancelled while clearing call state"),
            s.attemptId,
          );
          cancelMucCallPreparationWaiters(
            s.peer,
            s.selfNick,
            new Error("Muji preparing wait cancelled while clearing call state"),
          );
          $callState.set({ phase: "idle" });
          {
            const raw = sender as RawIqSender;
            if (s.selfNick && raw.update_muji_presence) {
              try {
                await raw.update_muji_presence(s.peer, s.selfNick, false, false, false, {
                  handRaised: false,
                  muted: false, // leaving clears all in-call state
                });
              } catch (err) {
                reportCallError(err);
              } finally {
                clearMucCallParticipant(s.peer, s.selfNick, s.selfFullJid);
                setSelfRaisedHand(s.peer, false);
              }
            }
            if (s.activePresencePublished) {
              try {
                await sendMujiSessionTerminate(raw, s.peer, s.sid);
                forgetMucCallSession({
                  roomJid: s.peer,
                  selfFullJid: s.selfFullJid,
                  sid: s.sid,
                });
              } catch (err) {
                reportCallError(err);
              }
            }
          }
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
    sid: string,
    video: boolean,
  ) => Promise<void>;
  send_muji_session_terminate?: (room_jid: string, sid: string) => Promise<void>;
  update_muji_presence?: (
    room_jid: string,
    nick: string,
    active: boolean,
    preparing: boolean,
    video: boolean,
    in_call: { handRaised: boolean; muted: boolean },
  ) => Promise<void>;
};

/**
 * XEP-0166 §6.3 + XEP-0272 separate-IQ accept flow:
 *
 *   1. Register a pending Muji-accept resolver keyed by `attemptId`
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
  attemptId: string,
  video: boolean,
  expectedMixerJid?: string | null,
): Promise<LiveKitJoin> {
  if (!sender.send_muji_session_initiate) {
    throw new Error(
      "wasm client does not expose send_muji_session_initiate; rebuild the wasm bundle",
    );
  }
  const acceptWait = registerPendingMujiAccept(
    attemptId,
    roomJid,
    attemptId,
    expectedMixerJid,
  );
  acceptWait.catch(() => undefined);
  try {
    await sender.send_muji_session_initiate(roomJid, attemptId, video);
  } catch (err) {
    // Roll back the pending registration so a future call attempt
    // doesn't resolve against this aborted one.
    rejectPendingMujiAccept(
      attemptId,
      err instanceof Error ? err : new Error(String(err)),
      attemptId,
    );
    throw err;
  }
  return await acceptWait;
}

async function rollbackMucCallSetup(
  sender: RawIqSender,
  roomJid: string,
  selfNick: string,
  selfFullJid: string | null | undefined,
  terminateSfu: boolean,
  sid?: string,
): Promise<void> {
  if (sender.update_muji_presence) {
    try {
      await sender.update_muji_presence(roomJid, selfNick, false, false, false, {
        handRaised: false,
        muted: false, // rollback clears all in-call state
      });
    } catch (err) {
      reportCallError(err);
    } finally {
      clearMucCallParticipant(roomJid, selfNick, selfFullJid);
    }
  }
  if (terminateSfu) {
    try {
      if (!sid) {
        throw new Error("Cannot terminate Muji session without the active Jingle sid");
      }
      await sendMujiSessionTerminate(sender, roomJid, sid);
      forgetMucCallSession({ roomJid, selfFullJid, sid });
    } catch (err) {
      reportCallError(err);
    }
  }
}

/**
 * Send a XEP-0272 Muji-bearing Jingle `session-terminate` so the
 * SFU unregisters us and revokes any tokens it minted for this
 * `(room, identity)` pair.
 */
async function sendMujiSessionTerminate(
  sender: RawIqSender | CallWireSender,
  roomJid: string,
  sid: string,
): Promise<void> {
  const rawSender = sender as RawIqSender;
  if (!rawSender.send_muji_session_terminate) {
    throw new Error(
      "wasm client does not expose send_muji_session_terminate; rebuild the wasm bundle",
    );
  }
  await rawSender.send_muji_session_terminate(roomJid, sid);
}

function mucSetupStillPending(
  roomJid: string,
  selfNick: string,
  attemptId: string,
): boolean {
  const state = $callState.get();
  return (
    state.phase === "muc-pending" &&
    state.peer === roomJid &&
    state.sid === attemptId &&
    state.selfNick === selfNick &&
    state.attemptId === attemptId
  );
}

function assertMucSetupStillPending(
  roomJid: string,
  selfNick: string,
  attemptId: string,
): void {
  if (!mucSetupStillPending(roomJid, selfNick, attemptId)) {
    throw new Error(`Muji call setup for ${roomJid} was cancelled`);
  }
}

/**
 * Originator action for MUC group calls. Implements XEP-0272
 * §Joining's two-phase flow:
 *
 *   1. Emit `<muji><preparing/></muji>` MUC presence so other
 *      occupants know we are about to join (the XEP MUSTs this).
 *   2. Wait for our preparing echo and for any other preparing
 *      occupants to finish, then publish active content presence.
 *   3. Send the Jingle session-initiate to the SFU mixer and wait
 *      for the separate session-accept carrying LiveKit credentials.
 *   4. Transition `$callState` to `active`.
 *
 * XEP-0272's MUST about waiting for the MUC rebroadcast of the
 * preparing presence is a setup precondition: if the echo or active
 * Muji publication fails, setup rolls back and the local call never
 * becomes active.
 *
 * No propose/proceed JMI round-trip — MUC calls are
 * presence-discovered, so there's no responder to ring.
 */
export async function beginMucCall(
  sender: RawIqSender,
  roomJid: string,
  media: CallMedia,
  selfNick?: string,
  expectedMixerJid?: string | null,
  selfFullJid?: string | null,
  lifecycleAttemptId: string = crypto.randomUUID(),
): Promise<void> {
  const attemptId = lifecycleAttemptId;
  const normalizedRoomJid = normalizeMucCallRoomJid(roomJid);
  if (!normalizedRoomJid) {
    reportFailedCallAttempt(attemptId, "muc");
    throw new Error("Cannot start a group call without a room JID");
  }
  // Fresh call: a hand left raised from a prior session in this room must
  // not silently re-raise on the new active presence (#1029).
  setSelfRaisedHand(normalizedRoomJid, false);
  if (!selfNick) {
    reportFailedCallAttempt(attemptId, "muc");
    throw new Error("Cannot start a group call before MUC presence has a nick");
  }
  if (!sender.update_muji_presence) {
    reportFailedCallAttempt(attemptId, "muc");
    throw new Error(
      "wasm client does not expose update_muji_presence; rebuild the wasm bundle",
    );
  }
  const current = $callState.get();
  if (current.phase !== "idle" && current.phase !== "ended") {
    reportFailedCallAttempt(attemptId, "muc");
    throw new Error("Cannot start a group call while another call is active or starting");
  }

  $callState.set({
    phase: "muc-pending",
    peer: normalizedRoomJid,
    sid: attemptId,
    media,
    kind: "muc",
    selfNick,
    selfFullJid,
    attemptId,
  });
  beginCallAttempt(attemptId, "muc");
  $lastCallError.set(null);

  // Step 1 — preparing presence (XEP-0272 §Joining two-phase flow).
  // Register the echo waiter BEFORE emitting so a fast MUC echo
  // can't fire before our listener is ready. The await below
  // blocks until the MUC rebroadcasts our preparing presence; the
  // timeout rejects and rolls setup back.
  let echoWait: Promise<void> | null = null;
  let activePresencePublished = false;
  try {
    echoWait = awaitPreparingEcho(
      normalizedRoomJid,
      selfNick,
      PREPARING_ECHO_TIMEOUT_MS,
      selfFullJid,
    );
    await sender.update_muji_presence(normalizedRoomJid, selfNick, false, true, false, {
      // in-call state isn't advertised before joining
      handRaised: false,
      muted: false,
    });
    assertMucSetupStillPending(normalizedRoomJid, selfNick, attemptId);
    // XEP-0272 §Joining: "The client MUST then wait until the MUC
    // rebroadcasts its presence message", then wait for other
    // occupants that had `<preparing/>` in their presence to finish
    // preparation before we publish contents.
    await echoWait;
    assertMucSetupStillPending(normalizedRoomJid, selfNick, attemptId);
    await awaitNoOtherPreparing(
      normalizedRoomJid,
      selfNick,
      PREPARING_PEERS_TIMEOUT_MS,
      selfFullJid,
    );
    assertMucSetupStillPending(normalizedRoomJid, selfNick, attemptId);

    // Step 2 — content-declaring presence. XEP-0272 §Joining says a
    // client advertises contents in MUC presence before initiating
    // the corresponding Jingle sessions.
    await sender.update_muji_presence(normalizedRoomJid, selfNick, true, false, media.video, {
      handRaised: selfRaisedHandFor(normalizedRoomJid), // preserve across re-emit
      // joining without mic capture advertises muted (#1030); a live mic
      // toggle re-broadcasts the authoritative state post-connect
      muted: !media.audio,
    });
    if (!mucSetupStillPending(normalizedRoomJid, selfNick, attemptId)) {
      await rollbackMucCallSetup(
        sender,
        normalizedRoomJid,
        selfNick,
        selfFullJid,
        false,
        attemptId,
      );
      throw new Error(`Muji call setup for ${normalizedRoomJid} was cancelled`);
    }
    activePresencePublished = true;
    $callState.set({
      phase: "muc-pending",
      peer: normalizedRoomJid,
      sid: attemptId,
      media,
      kind: "muc",
      selfNick,
      selfFullJid,
      attemptId,
      activePresencePublished: true,
    });
    // Step 3 — session-initiate. The SFU mixer's session-accept
    // arrives after the active Muji presence is already room-visible.
    const join = await sendMujiSessionInitiate(
      sender,
      normalizedRoomJid,
      attemptId,
      media.video,
      expectedMixerJid,
    );
    assertMucSetupStillPending(normalizedRoomJid, selfNick, attemptId);

    $callState.set({
      phase: "active",
      peer: normalizedRoomJid,
      sid: attemptId,
      media,
      join,
      kind: "muc",
      selfNick,
      selfFullJid,
    });
    markCallAttemptAccepted(attemptId);
    rememberMucCallSession({
      roomJid: normalizedRoomJid,
      sid: attemptId,
      selfFullJid,
      media,
      join,
    });
    $lastCallError.set(null);
  } catch (err) {
    echoWait?.catch(() => undefined);
    const pending = $callState.get();
    if (
      pending.phase === "muc-pending" &&
      pending.peer === normalizedRoomJid &&
      pending.sid === attemptId &&
      pending.attemptId === attemptId
    ) {
      await rollbackMucCallSetup(
        sender,
        normalizedRoomJid,
        selfNick,
        selfFullJid,
        activePresencePublished,
        attemptId,
      );
      finishCallAttempt(attemptId, { setupOutcome: "failed", endReason: "error" });
      $callState.set({ phase: "idle" });
    }
    throw err;
  }
}
