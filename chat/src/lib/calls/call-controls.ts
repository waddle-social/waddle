import { atom } from "nanostores";
import {
  $callState,
  clearCallState,
  reportCallError,
  tearDownActiveCall,
} from "./call-store";
import type { CallWireSender } from "./outbound";
import { connectionStore } from "@/lib/connection-store";
import { useCallEngine } from "./use-call-engine";

/**
 * Shared mic / cam / connecting state for the in-call surfaces.
 *
 * Lifted from CallOverlay's local refs into module scope so the
 * split-view container, the fullscreen overlay, and the lifecycle
 * host can all read and write the same source of truth without a
 * provide/inject chain (the surfaces live in detached subtrees).
 *
 * The atoms here mirror the engine: when the user toggles mic via
 * the split-view control bar, the engine `setMicEnabled(false)` and
 * the atom flips at the same time. On failure the atom rolls back.
 */
export const $callConnecting = atom<boolean>(false);
export const $callMicEnabled = atom<boolean>(true);
export const $callCamEnabled = atom<boolean>(true);

function getSender(): CallWireSender | null {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as CallWireSender | undefined) ?? null;
}

/** Toggle the local microphone, with optimistic UI + rollback. */
export async function toggleMic(): Promise<void> {
  const next = !$callMicEnabled.get();
  $callMicEnabled.set(next);
  try {
    const { engine } = useCallEngine();
    await engine.setMicEnabled(next);
  } catch (err) {
    $callMicEnabled.set(!next);
    reportCallError(err);
  }
}

/** Toggle the local camera, with optimistic UI + rollback. */
export async function toggleCam(): Promise<void> {
  const next = !$callCamEnabled.get();
  $callCamEnabled.set(next);
  try {
    const { engine } = useCallEngine();
    await engine.setCameraEnabled(next);
  } catch (err) {
    $callCamEnabled.set(!next);
    reportCallError(err);
  }
}

/**
 * Graceful hangup. Delegates to `tearDownActiveCall` so the
 * appropriate XEP-0166 / XEP-0272 stanzas go out before the engine
 * disconnects.
 */
export async function hangupActiveCall(): Promise<void> {
  await tearDownActiveCall(getSender(), "success");
  try {
    const { engine } = useCallEngine();
    await engine.disconnect();
  } catch (err) {
    reportCallError(err);
  }
}

/**
 * Browser page lifecycle path. A full tab refresh emits `pagehide`
 * before the new JS context reconnects; sending XEP-0166
 * `<session-terminate/>` or XEP-0272 clearing presence here destroys
 * the call the refreshed page is about to rediscover. Release local
 * media only. Explicit hangup/logout still goes through
 * `tearDownActiveCall` and sends the conformant call-ending stanzas.
 */
export function suspendCallForPageHide(): void {
  const current = $callState.get();
  if (current.phase === "idle" || current.phase === "ended") return;
  clearCallState();
  void useCallEngine().engine.disconnect();
}

/**
 * Reset mic/cam to a known-good baseline when a fresh call begins.
 * The engine snapshot is the source of truth — pass the values the
 * call started with so an unanswered mute or camera-off from a
 * previous call doesn't leak across.
 */
export function resetCallControls(micEnabled: boolean, camEnabled: boolean): void {
  $callMicEnabled.set(micEnabled);
  $callCamEnabled.set(camEnabled);
}

/** Read the live call's peer label (`#room` for MUC, localpart for DM). */
export function peerLabelFromState(): string {
  const s = $callState.get();
  if (s.phase !== "active") return "";
  const at = s.peer.indexOf("@");
  const localpart = at > 0 ? s.peer.slice(0, at) : s.peer;
  return s.kind === "muc" ? `#${localpart}` : localpart;
}
