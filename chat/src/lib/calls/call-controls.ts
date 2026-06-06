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
import {
  clearAllMediaIssues,
  clearMediaIssue,
  recordMediaIssue,
} from "./call-media-issues";
import {
  $callScreenShareEnabled,
  $callScreenShareSupported,
  refreshScreenShareSupported,
} from "./screen-share-state";

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
export { $callScreenShareEnabled, $callScreenShareSupported, refreshScreenShareSupported };

function isScreenSharePickerCancel(error: unknown): boolean {
  return !!error &&
    typeof error === "object" &&
    "name" in error &&
    (error as { name?: unknown }).name === "NotAllowedError";
}

function getSender(): CallWireSender | null {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as CallWireSender | undefined) ?? null;
}

/**
 * Toggle the local microphone, with optimistic UI + rollback. Doubles
 * as the recovery path: a device-less / permission-denied participant
 * who later plugs in a mic or grants access can click "unmute" to
 * retry capture. On success the recorded media issue is cleared; on
 * failure the atom rolls back and the issue is (re)recorded so the
 * non-blocking notice updates instead of a transient error toast.
 */
export async function toggleMic(): Promise<void> {
  const next = !$callMicEnabled.get();
  $callMicEnabled.set(next);
  try {
    const { engine } = useCallEngine();
    await engine.setMicEnabled(next);
    clearMediaIssue("mic");
  } catch (err) {
    $callMicEnabled.set(!next);
    recordMediaIssue("mic", err);
  }
}

/** Toggle the local camera, with optimistic UI + rollback. Mirrors
 *  [`toggleMic`] — also the retry path for a device-less / denied
 *  camera once a device is attached or access is granted. */
export async function toggleCam(): Promise<void> {
  const next = !$callCamEnabled.get();
  $callCamEnabled.set(next);
  try {
    const { engine } = useCallEngine();
    await engine.setCameraEnabled(next);
    clearMediaIssue("cam");
  } catch (err) {
    $callCamEnabled.set(!next);
    recordMediaIssue("cam", err);
  }
}

export async function toggleScreenShare(): Promise<void> {
  const next = !$callScreenShareEnabled.get();
  $callScreenShareEnabled.set(next);
  try {
    const { engine } = useCallEngine();
    await engine.setScreenShareEnabled(next, { audio: next });
    clearMediaIssue("screen");
  } catch (err) {
    $callScreenShareEnabled.set(!next);
    if (next && isScreenSharePickerCancel(err)) return;
    recordMediaIssue("screen", err);
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
  (connectionStore.client as unknown as {
    persistResumeStateForPageHide?: () => void;
  } | null)?.persistResumeStateForPageHide?.();
  const current = $callState.get();
  if (current.phase === "idle" || current.phase === "ended") return;
  clearCallState();
  void useCallEngine().engine.disconnect();
}

type PagehideTarget = Pick<Window, "addEventListener" | "removeEventListener">;

export function installCallPagehideSuspension(windowTarget: PagehideTarget = window): () => void {
  const handlePageHide = (event: PageTransitionEvent): void => {
    if (event.persisted) return;
    suspendCallForPageHide();
  };

  windowTarget.addEventListener("pagehide", handlePageHide as EventListener);
  return () => {
    windowTarget.removeEventListener("pagehide", handlePageHide as EventListener);
  };
}

/**
 * Reset mic/cam to a known-good baseline when a fresh call begins.
 * Pass the values the call started with so an unanswered mute or
 * camera-off from a previous call doesn't leak across, and drop any
 * media issues left over from the previous call.
 *
 * This is the OPTIMISTIC baseline shown while connecting;
 * [`seedCallControlsFromEngine`] reconciles to the actual published
 * state once the engine has connected.
 */
export function resetCallControls(micEnabled: boolean, camEnabled: boolean): void {
  $callMicEnabled.set(micEnabled);
  $callCamEnabled.set(camEnabled);
  $callScreenShareEnabled.set(false);
  clearAllMediaIssues();
}

/**
 * Reconcile the mic/cam toggle atoms to the engine's ACTUAL published
 * state after a connect. The initial capture is best-effort, so the
 * requested media booleans can lie (no device / denied permission);
 * LiveKit's local participant is the source of truth. Without this the
 * toggle could show "mic on" while nothing is actually publishing.
 */
export function seedCallControlsFromEngine(engine: {
  micEnabled: boolean;
  cameraEnabled: boolean;
  screenShareEnabled?: boolean;
}): void {
  $callMicEnabled.set(engine.micEnabled);
  $callCamEnabled.set(engine.cameraEnabled);
  $callScreenShareEnabled.set(engine.screenShareEnabled ?? false);
}

/** Read the live call's peer label (`#room` for MUC, localpart for DM). */
export function peerLabelFromState(): string {
  const s = $callState.get();
  if (s.phase !== "active") return "";
  const at = s.peer.indexOf("@");
  const localpart = at > 0 ? s.peer.slice(0, at) : s.peer;
  return s.kind === "muc" ? `#${localpart}` : localpart;
}
