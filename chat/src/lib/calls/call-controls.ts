import { atom } from "nanostores";
import {
  $callState,
  clearCallState,
  reportCallError,
  tearDownActiveCall,
  type RawIqSender,
} from "./call-store";
import { broadcastMucCallSelfMute } from "./muc-call-actions";
import { $callMicEnabled } from "./call-mic-state";
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
 *
 * `$callMicEnabled` is owned by the `call-mic-state` leaf (so the
 * presence broadcaster in `muc-call-actions` can read it without a
 * module cycle back into this file) and re-exported here for the many
 * control-bar consumers that import it from `call-controls`.
 */
export const $callConnecting = atom<boolean>(false);
export const $callCamEnabled = atom<boolean>(true);
export { $callMicEnabled };
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
 * Serialize mic enable/disable (#1034). Push-to-talk fires `setCallMic`
 * back-to-back on Space down/up; without ordering, a late-resolving enable
 * could land after a newer disable and strand the mic open (and broadcast a
 * stale mute). Each request bumps a generation token and chains behind the
 * previous op, so engine calls never overlap and a request that is no longer
 * the latest is dropped — before the engine call if it never started, or
 * before its post-effects if it was superseded mid-flight. Only the last
 * requested state is applied and broadcast.
 */
let micOpChain: Promise<void> = Promise.resolve();
let micRequestGen = 0;
let camOpChain: Promise<void> = Promise.resolve();
let camRequestGen = 0;
let screenShareOpChain: Promise<void> = Promise.resolve();
let screenShareRequestGen = 0;

/**
 * Drive the local microphone to `next` with optimistic UI + rollback, the
 * recorded media-issue notice, and the `urn:waddle:in-call:0` mute broadcast —
 * serialized last-writer-wins (see [`micOpChain`]). Doubles as the recovery
 * path: a device-less / permission-denied participant who later plugs in a mic
 * or grants access can retry capture. Resolves once this request's effects
 * have settled (or it was superseded).
 */
function setCallMic(next: boolean): Promise<void> {
  const gen = ++micRequestGen;
  $callMicEnabled.set(next); // optimistic
  const op = micOpChain.then(async () => {
    if (gen !== micRequestGen) return; // a newer request superseded this one
    try {
      const { engine } = useCallEngine();
      await engine.setMicEnabled(next);
      if (gen !== micRequestGen) return; // superseded while awaiting the engine
      clearMediaIssue("mic");
      // #1030: broadcast the new mute state as `urn:waddle:in-call:0`
      // presence so other participants' tiles and roster reflect it — the
      // authoritative, XMPP-native remote-mute path (no LiveKit data
      // channel). A no-op for DM calls; the broadcaster guards on an active
      // MUC call. Fire-and-forget: a failed presence self-heals on the next.
      const sender = getSender();
      if (sender) void broadcastMucCallSelfMute(sender as unknown as RawIqSender, !next);
    } catch (err) {
      if (gen === micRequestGen) {
        $callMicEnabled.set(!next);
        recordMediaIssue("mic", err);
      }
    }
  });
  micOpChain = op;
  return op;
}

/**
 * Toggle the local microphone. Thin wrapper over [`setCallMic`] so manual
 * mute clicks share the same serialized, optimistic, rollback-and-broadcast
 * path as push-to-talk.
 */
export async function toggleMic(): Promise<void> {
  await setCallMic(!$callMicEnabled.get());
}

/**
 * Push-to-talk (#1034): force the mic on while the Space key is held and
 * off on release, via the serialized [`setCallMic`]. A no-op when the mic is
 * already in the requested state, so a held/repeating key or an already-open
 * mic doesn't churn the engine or the mute presence.
 */
export function setPushToTalkActive(active: boolean): void {
  if ($callMicEnabled.get() !== active) void setCallMic(active);
}

/** Toggle the local camera, with optimistic UI + rollback. Mirrors
 *  [`toggleMic`] — also the retry path for a device-less / denied
 *  camera once a device is attached or access is granted. */
export async function toggleCam(): Promise<void> {
  const next = !$callCamEnabled.get();
  const gen = ++camRequestGen;
  $callCamEnabled.set(next);
  const op = camOpChain.then(async () => {
    if (gen !== camRequestGen) return;
    try {
      const { engine } = useCallEngine();
      await engine.setCameraEnabled(next);
      if (gen !== camRequestGen) return;
      clearMediaIssue("cam");
    } catch (err) {
      if (gen !== camRequestGen) return;
      $callCamEnabled.set(!next);
      recordMediaIssue("cam", err);
    }
  });
  camOpChain = op;
  await op;
}

export async function toggleScreenShare(): Promise<void> {
  const next = !$callScreenShareEnabled.get();
  const gen = ++screenShareRequestGen;
  $callScreenShareEnabled.set(next);
  const op = screenShareOpChain.then(async () => {
    if (gen !== screenShareRequestGen) return;
    try {
      const { engine } = useCallEngine();
      await engine.setScreenShareEnabled(next, { audio: next });
      if (gen !== screenShareRequestGen) return;
      clearMediaIssue("screen");
    } catch (err) {
      if (gen !== screenShareRequestGen) return;
      $callScreenShareEnabled.set(!next);
      if (next && isScreenSharePickerCancel(err)) return;
      recordMediaIssue("screen", err);
    }
  });
  screenShareOpChain = op;
  await op;
}

/**
 * Graceful hangup. Media first, protocol second (#1446): stop local
 * capture and the LiveKit transport immediately, then let
 * `tearDownActiveCall` send the XEP-0166 / XEP-0272 stanzas in the
 * background. Hang-up must never wait on an XMPP round-trip to turn
 * the camera off — a server that never replies would leave the user
 * live on mic/camera after clicking "hang up". The wire teardown is
 * bounded and best-effort; its failure never re-opens media.
 */
export async function hangupActiveCall(): Promise<void> {
  const sender = getSender();
  const engineDisconnected = (async () => {
    try {
      await useCallEngine().engine.disconnect();
    } catch (err) {
      reportCallError(err);
    }
  })();
  // Started AFTER the engine disconnect so capture release is
  // initiated first, but not awaited behind it: the synchronous
  // prefix flips the call slot to idle immediately, and the bounded
  // wire teardown proceeds concurrently — a LiveKit disconnect
  // stalling on a dead network can't pin the UI on the ended call
  // or delay the terminate stanzas.
  void tearDownActiveCall(sender, "success").catch(reportCallError);
  await engineDisconnected;
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
  // This page's media connection is ending even when a refreshed page later
  // rediscovers the protocol call. A rediscovery is a distinct connection
  // attempt and owns its own lifecycle event.
  clearCallState({ endReason: "error" });
  void useCallEngine().engine.disconnect();
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
  micRequestGen++;
  camRequestGen++;
  screenShareRequestGen++;
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
  // #1030: capture is best-effort, so the join-time mute advertisement
  // (derived from the REQUESTED media) can be wrong — device missing,
  // permission denied, or the user joined muted. Now that `$callMicEnabled`
  // reflects the ACTUAL published mic, re-broadcast the true mute so peers'
  // tiles/roster are accurate. No-op for DM calls; the broadcaster guards on
  // an active MUC call, and `getSender()` is null outside a live connection.
  const sender = getSender();
  if (sender) void broadcastMucCallSelfMute(sender as unknown as RawIqSender, !engine.micEnabled);
}

/** Read the live call's peer label (`#room` for MUC, localpart for DM). */
export function peerLabelFromState(): string {
  const s = $callState.get();
  if (s.phase !== "active") return "";
  const at = s.peer.indexOf("@");
  const localpart = at > 0 ? s.peer.slice(0, at) : s.peer;
  return s.kind === "muc" ? `#${localpart}` : localpart;
}
