import { atom } from "nanostores";
import type { CallEvent, CallMedia, CallState } from "./types";

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
 * Pure reducer over the current call state. Exposed for unit
 * testing; the production caller is the `on_call` registration in
 * `lib/xmpp/client.ts`, which subscribes via `applyCallEvent`.
 */
export function reduceCallState(current: CallState, event: CallEvent): CallState {
  switch (event.kind) {
    case "propose":
      // The remote peer is ringing us — surface as incoming so the
      // toast can offer accept/reject. If we're already in a call,
      // ignore (XEP-0353 expects clients to reject with `<busy/>`,
      // which the UI will handle separately).
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
      // `outgoing` until session-accept lands; the side-effect
      // handler in `call-effects.ts` reads `proceed.from` (the
      // responder's full JID) and emits the session-initiate stanza.
      return current;
    case "reject":
    case "retract":
      // Either side aborted before media. Always wins.
      if (current.phase !== "idle") {
        return { phase: "ended", sid: event.sid, reason: event.kind };
      }
      return current;
    case "session-initiate":
    case "session-accept":
      return {
        phase: "active",
        peer: event.from,
        sid: event.sid,
        media: event.media,
        join: event.join,
      };
    case "finish":
    case "session-terminate":
      return {
        phase: "ended",
        sid: event.sid,
        reason: event.kind === "session-terminate" ? event.reason : null,
      };
  }
}

/**
 * Apply an inbound `CallEvent` from the WASM `on_call` callback to
 * the global store. Idempotent for ignored events.
 */
export function applyCallEvent(event: CallEvent): void {
  $callState.set(reduceCallState($callState.get(), event));
}

/**
 * Mark the local call slot as idle. Called when the UI dismisses
 * the `ended` state (the toast closes, the user clicks dismiss).
 */
export function clearCallState(): void {
  $callState.set({ phase: "idle" });
}

/**
 * Originator-side transition: snapshot the outbound `<propose/>`
 * intent into the store so the UI can render the outgoing-ring
 * overlay. The wire send is the caller's responsibility — keep
 * the store side-effect free for clean unit testing.
 */
export function beginOutgoingCall(to: string, sid: string, media: CallMedia): void {
  $callState.set({ phase: "outgoing", to, sid, media });
}
