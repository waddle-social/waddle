import { ref, type Ref } from "vue";
import { $callState, clearCallState, reportCallError } from "./call-store";
import { DisconnectReason } from "livekit-client";
import { CallEngine, type LocalMediaTrack, type RemoteMediaTrack } from "./engine";
import { applyCallDeviceSelection } from "./call-device-selection";
import {
  advanceActiveSpeakers,
  emptyActiveSpeakerState,
  evictActiveSpeaker,
  type ActiveSpeakerState,
} from "./active-speakers";
import {
  advanceSpeakerPromotion,
  emptySpeakerPromotion,
  type SpeakerPromotionState,
} from "./view-mode";
import {
  addLiveCallParticipant,
  clearLiveCallParticipants,
  markRoomLeavingCall,
  removeLiveCallParticipant,
  setLiveCallParticipants,
} from "./muc-call-live-participants";
import { clearAllMediaIssues, recordMediaIssue } from "./call-media-issues";
import { enumerateCallDevices, hasEnumeratedCallDeviceId } from "./device-prefs";
import { syncScreenShareEnabled } from "./screen-share-state";
import { setCallAudioPlaybackBlocked } from "./call-audio-playback";
import { resetMicAudioProcessing, setMicAudioProcessing } from "./mic-audio-processing-state";
import { resetMicAiNoiseFilter, setMicAiNoiseFilter } from "./mic-ai-noise-filter-state";
import {
  clearAiNoiseFilterError,
  setAiNoiseFilterError,
} from "./ai-noise-filter-error-state";
import { resetCameraBackground, setCameraBackground } from "./camera-background-effect-state";
import {
  clearBackgroundEffectError,
  setBackgroundEffectError,
} from "./background-effect-error-state";
import { createCallAudioProcessingBeacon } from "./call-audio-processing-telemetry";
import {
  createCallMediaPathBeacon,
  type CallMediaPathSnapshot,
} from "./call-media-path-telemetry";
import { createCallIceBeacon, iceStatsEntries } from "./call-ice-telemetry";
import { adoptCallCorrelationId, clearCallCorrelationId } from "./call-correlation";
import {
  audioBitrateBand,
  summarizeAudioStats,
  summarizeVideoStats,
  videoResolutionBand,
  type CallStatDirection,
  type CallStatSample,
} from "./call-stats";
import {
  reportCallAudioProcessing,
  reportCallIce,
  reportCallIceCredentials,
  reportCallMediaPath,
} from "../telemetry";
import {
  resetCallConnectionQuality,
  setCallConnectionPhase,
  setCallConnectionQuality,
} from "./connection-quality";
import { resetCallActiveSince, setCallActiveSince } from "./call-duration";
import {
  observeCallConnectionPhase,
  observeCallConnectionQuality,
  observeCallStats,
  finishCallAttemptForTransportDisconnect,
  type CallKind,
} from "./call-lifecycle-telemetry";

function transportDisconnectMessage(reason?: DisconnectReason): string {
  switch (reason) {
    case DisconnectReason.DUPLICATE_IDENTITY:
      return "This call ended because the same account joined from another device.";
    case DisconnectReason.PARTICIPANT_REMOVED:
      return "You were removed from the call.";
    case DisconnectReason.ROOM_DELETED:
    case DisconnectReason.ROOM_CLOSED:
      return "The call room was closed.";
    case DisconnectReason.SERVER_SHUTDOWN:
      return "The call service shut down unexpectedly.";
    case DisconnectReason.USER_UNAVAILABLE:
    case DisconnectReason.USER_REJECTED:
      return "The other participant is no longer available for this call.";
    case DisconnectReason.CONNECTION_TIMEOUT:
      return "The call connection timed out.";
    case DisconnectReason.MEDIA_FAILURE:
      return "The call ended because the media connection failed.";
    case DisconnectReason.JOIN_FAILURE:
      return "The call ended because the room connection failed.";
    default:
      return "The call connection was lost.";
  }
}

/**
 * Process-wide singleton: only one call engine should ever exist
 * because the call-store only tracks one call at a time. The Vue
 * tree may mount/unmount the overlay component repeatedly during a
 * call; tying the engine to the component lifecycle would tear it
 * down on every re-render, so the engine outlives the component.
 */
let singletonEngine: CallEngine | null = null;
const remoteTracks: Ref<RemoteMediaTrack[]> = ref([]);
/**
 * Local tracks the user has currently published (camera, mic, …).
 * Drives the self-preview tile in `CallOverlay`. Without this the
 * user only sees other participants — the most common call-UI bug
 * report ("can't see my own camera"). Symmetric with `remoteTracks`
 * so the renderer can use one tile component for both.
 */
const localTracks: Ref<LocalMediaTrack[]> = ref([]);
/**
 * Identities currently highlighted as the active speaker, derived from
 * LiveKit's `ActiveSpeakersChanged` signal through the pure
 * `advanceActiveSpeakers` hold logic. Drives the speaker highlight border on
 * Gallery tiles. A `ReadonlySet` so the renderer membership-checks per tile.
 */
const activeSpeakerIdentities: Ref<ReadonlySet<string>> = ref(new Set());
/** Hold state for the active-speaker derivation; reset between calls. */
let activeSpeakerState: ActiveSpeakerState = emptyActiveSpeakerState();
/**
 * The speaking set from LiveKit's most recent event. The scheduled release
 * sweep re-runs the derivation against this same set so a held highlight clears
 * on time even when LiveKit fires no further events (continuous silence).
 */
let lastSpeakingIdentities: readonly string[] = [];
/** Single pending release sweep (`setTimeout`), armed at the next deadline. */
let activeSpeakerSweep: ReturnType<typeof setTimeout> | null = null;

/**
 * The participant the Speaker layout auto-promotes to the large tile, derived
 * from the held active-speaker set through the sticky `advanceSpeakerPromotion`
 * machine: it holds the current speaker through silence and only hands the
 * large tile to a different participant once they become active. Consumed only
 * in Speaker view; a pin or a screen share outranks it (see `projectCallTiles`).
 */
const promotedSpeakerIdentity: Ref<string | null> = ref(null);
/** Sticky promotion state for the derivation; reset between calls. */
let speakerPromotion: SpeakerPromotionState = emptySpeakerPromotion();

/**
 * Re-derive the highlighted set against `lastSpeakingIdentities` at the current
 * wall-clock time, publish it, and (re)arm a single sweep for the next release
 * deadline. Idempotent: safe to call from a LiveKit event or from the sweep.
 */
function pumpActiveSpeakers(): void {
  if (activeSpeakerSweep) {
    clearTimeout(activeSpeakerSweep);
    activeSpeakerSweep = null;
  }
  // One clock read for both the derivation and the sweep delay, so the timer is
  // armed exactly relative to the `now` that produced `nextDeadline` — two reads
  // can drift across a slow frame and fire a spurious 0 ms sweep.
  const now = Date.now();
  const step = advanceActiveSpeakers({
    state: activeSpeakerState,
    speakingIdentities: lastSpeakingIdentities,
    now,
  });
  activeSpeakerState = step.state;
  activeSpeakerIdentities.value = step.activeIdentities;
  speakerPromotion = advanceSpeakerPromotion({
    state: speakerPromotion,
    activeIdentities: step.activeIdentities,
  });
  promotedSpeakerIdentity.value = speakerPromotion.promotedIdentity;
  if (step.nextDeadline !== null) {
    activeSpeakerSweep = setTimeout(pumpActiveSpeakers, Math.max(0, step.nextDeadline - now));
  }
}

/**
 * Purge a departed participant from all active-speaker state — the brief hold
 * AND the sticky Speaker-view promotion — then re-derive. Without this, the ~1s
 * release hold (or the sticky promotion, which holds the last speaker through
 * silence) would survive their departure and re-highlight or re-promote them
 * the instant they rejoin with a fresh tile.
 */
function forgetDepartedSpeaker(identity: string): void {
  if (speakerPromotion.promotedIdentity === identity) {
    speakerPromotion = emptySpeakerPromotion();
  }
  activeSpeakerState = evictActiveSpeaker(activeSpeakerState, identity);
  lastSpeakingIdentities = lastSpeakingIdentities.filter((id) => id !== identity);
  pumpActiveSpeakers();
}

/** Drop all active-speaker state so the next call starts from a clean slate. */
function resetActiveSpeakers(): void {
  if (activeSpeakerSweep) {
    clearTimeout(activeSpeakerSweep);
    activeSpeakerSweep = null;
  }
  activeSpeakerState = emptyActiveSpeakerState();
  lastSpeakingIdentities = [];
  activeSpeakerIdentities.value = new Set();
  speakerPromotion = emptySpeakerPromotion();
  promotedSpeakerIdentity.value = null;
}

/**
 * Fleet-measurement beacon for the verified mic audio-processing state
 * (#913). De-dupes to at most one Faro event per call per distinct state;
 * no-op when Faro isn't configured. Reset on disconnect so the next call
 * re-arms from scratch.
 */
function currentCallKind(): CallKind | null {
  const state = $callState.get();
  if (state.phase === "active" || state.phase === "muc-pending") return state.kind;
  if (state.phase === "incoming" || state.phase === "outgoing") return "dm";
  return null;
}

const micAudioProcessingBeacon = createCallAudioProcessingBeacon((state) => {
  const callKind = currentCallKind();
  if (callKind) reportCallAudioProcessing(state, callKind);
});

/**
 * Fleet-measurement beacon for the media path each call track actually got
 * (#996): negotiated video codec + the succeeded ICE candidate-pair. Fed by a
 * lightweight read-only `getRTCStatsReport` poll that runs for the whole call
 * (independent of the diagnostics dialog), so the fleet baseline — and the
 * silent "stuck on TCP relay" rate — is captured on every call. De-dupes to at
 * most one event per distinct path per call; reset on disconnect.
 */
const callMediaPathBeacon = createCallMediaPathBeacon((snapshot) => {
  const callKind = currentCallKind();
  if (callKind) reportCallMediaPath(snapshot, callKind);
});

/**
 * Fleet-measurement beacon for the call's ICE/TURN connectivity (#1452):
 * relay-vs-direct media path, whether a TURN relay candidate was gathered at
 * all, and how many times ICE restarted. Fed by the same read-only stats poll
 * as the media-path beacon, plus the engine's `transportReconnecting`
 * event (a full media-transport reconnect is an ICE restart; signaling
 * WebSocket recovery is deliberately excluded). De-dupes to at most
 * one event per distinct ICE state per call; reset on disconnect.
 */

const callIceBeacon = createCallIceBeacon((snapshot) => {
  const callKind = currentCallKind();
  if (callKind) reportCallIce(snapshot, callKind);
}, (event) => {
  const callKind = currentCallKind();
  if (callKind) reportCallIceCredentials(event, callKind);
});

/** Cadence of the observational media-path poll. Codec/ICE settle within the
 * first seconds and then rarely change, and the beacon de-dupes, so a slow 5 s
 * poll captures the path and any mid-call re-route at negligible cost. */
const MEDIA_PATH_SAMPLE_INTERVAL_MS = 5_000;
let mediaPathTimer: ReturnType<typeof setInterval> | null = null;
// Guards against overlapping ticks if a `getStats()` pass outlives the interval.
let mediaPathSampling = false;
// Bumped on every start/stop. A tick captures the generation at launch and
// re-checks it after its awaits before observing, so an in-flight pass that
// outlives a disconnect (or a switch to another call) can never re-populate the
// just-reset beacon — which would otherwise suppress the next call's first
// event when both calls share a codec/ICE path.
let mediaPathGeneration = 0;

const CALL_DEVICE_CHANGE_DEBOUNCE_MS = 200;
let unsubscribeCallDeviceChange: (() => void) | null = null;
let callDeviceChangeTimer: ReturnType<typeof setTimeout> | null = null;
let callDeviceChangeGeneration = 0;
let pendingCallDeviceChange: { micId: string | null; camId: string | null } | null = null;

function missingActiveCallDeviceError(kind: "mic" | "cam"): Error {
  const error = new Error(`${kind} device was removed during the call`);
  error.name = "NotFoundError";
  return error;
}

async function reconcileRemovedActiveDevices(
  engine: CallEngine,
  generation: number,
  activeBeforeChange: { micId: string | null; camId: string | null },
): Promise<void> {
  const devices = await enumerateCallDevices();
  if (generation !== callDeviceChangeGeneration) return;

  const activeMicId = activeBeforeChange.micId;
  if (
    activeMicId &&
    activeMicId !== "default" &&
    !hasEnumeratedCallDeviceId(devices, "mic", activeMicId)
  ) {
    recordMediaIssue("mic", missingActiveCallDeviceError("mic"));
    await applyCallDeviceSelection("mic", null, engine);
    if (generation !== callDeviceChangeGeneration) return;
  }

  const activeCamId = activeBeforeChange.camId;
  if (
    activeCamId &&
    activeCamId !== "default" &&
    !hasEnumeratedCallDeviceId(devices, "cam", activeCamId)
  ) {
    recordMediaIssue("cam", missingActiveCallDeviceError("cam"));
    await applyCallDeviceSelection("cam", null, engine);
  }
}

function stopCallDeviceChangeListener(): void {
  callDeviceChangeGeneration += 1;
  if (callDeviceChangeTimer) {
    clearTimeout(callDeviceChangeTimer);
    callDeviceChangeTimer = null;
  }
  pendingCallDeviceChange = null;
  unsubscribeCallDeviceChange?.();
  unsubscribeCallDeviceChange = null;
}

function startCallDeviceChangeListener(engine: CallEngine): void {
  stopCallDeviceChangeListener();
  const generation = callDeviceChangeGeneration;
  if (typeof navigator === "undefined" || !navigator.mediaDevices?.addEventListener) return;
  const handler = () => {
    // Capture BEFORE the debounce. LiveKit registers its own devicechange
    // listener when the Room is built and may auto-switch activeDeviceMap to
    // the first remaining device before our async enumeration finishes. A
    // later read would then hide that the in-use device was removed.
    const snapshot = {
      micId: engine.activeDeviceId("audioinput"),
      camId: engine.activeDeviceId("videoinput"),
    };
    pendingCallDeviceChange = {
      micId: pendingCallDeviceChange?.micId ?? snapshot.micId,
      camId: pendingCallDeviceChange?.camId ?? snapshot.camId,
    };
    if (callDeviceChangeTimer) clearTimeout(callDeviceChangeTimer);
    callDeviceChangeTimer = setTimeout(() => {
      callDeviceChangeTimer = null;
      const activeBeforeChange = pendingCallDeviceChange;
      pendingCallDeviceChange = null;
      if (!activeBeforeChange) return;
      void reconcileRemovedActiveDevices(engine, generation, activeBeforeChange)
        .catch(reportCallError);
    }, CALL_DEVICE_CHANGE_DEBOUNCE_MS);
  };
  navigator.mediaDevices.addEventListener("devicechange", handler);
  unsubscribeCallDeviceChange = () => {
    navigator.mediaDevices.removeEventListener("devicechange", handler);
  };
}
// Previous byte/timestamp sample per audio track, so the media-path poll can
// derive the send/recv bitrate (a rate needs two samples) and band it. Keyed by
// the same track key used in the diagnostics dialog; cleared between calls in
// `stopMediaPathPolling`. Committed only after the generation re-check so a
// stale in-flight pass can't seed the next call's bitrate.
const audioMediaPathSamples = new Map<string, CallStatSample>();

/**
 * Read one video track's live stats and reduce them to a media-path snapshot,
 * or `null` when nothing has been negotiated yet (so a not-yet-connected track
 * never beacons) or the report is unavailable. `getRTCStatsReport()` rejects
 * mid-teardown, so a failure drops that single track rather than the tick.
 */
async function sampleTrackMediaPath(
  track: LocalMediaTrack | RemoteMediaTrack,
  direction: CallStatDirection,
): Promise<{ snapshot: CallMediaPathSnapshot | null; rttMs: number | null; packetLossPct: number | null } | null> {
  try {
    const report = await track.track.getRTCStatsReport();
    if (!report) return null;
    const { summary } = summarizeVideoStats(report, direction);
    if (summary.codec === null && summary.iceCandidateType === null) {
      return { snapshot: null, rttMs: summary.rttMs, packetLossPct: summary.packetLossPct };
    }
    return { snapshot: {
      direction,
      source: track.source === "screen_share" ? "screen" : "camera",
      codec: summary.codec,
      iceCandidateType: summary.iceCandidateType,
      iceTransport: summary.iceTransport,
      audioBitrateBand: null, // video paths carry no audio band
      // The negotiated top-layer resolution bucket: a fleet shift from 1080p to
      // 720p is the camera-cap egress signal (#1001).
      videoResolutionBand: videoResolutionBand(summary.resolution),
    }, rttMs: summary.rttMs, packetLossPct: summary.packetLossPct };
  } catch {
    return null;
  }
}

/** One audio track's media-path sample, carrying the fresh byte/timestamp
 *  sample to retain so the next poll can derive a bitrate. */
type AudioMediaPathResult = {
  snapshot: CallMediaPathSnapshot | null;
  key: string;
  sample: CallStatSample | null;
  rttMs: number | null;
  packetLossPct: number | null;
};

/**
 * Read one mic track's live stats and reduce them to an audio media-path
 * snapshot (negotiated codec, ICE path, and the active-speaker bitrate band),
 * plus the fresh sample to retain. The snapshot is `null` until a bitrate can be
 * measured (the first poll has no prior sample) — codec/ICE then ride along with
 * the band on the next tick. `getRTCStatsReport()` rejects mid-teardown, so a
 * failure drops this track rather than the whole pass.
 *
 * Direction matters when reading the band downstream: a `send` band is OUR
 * publish (direct evidence the raised default took effect); a `recv` band is a
 * remote peer's publish (whatever client they run), disambiguated by the
 * snapshot's `direction`.
 */
async function sampleAudioTrackMediaPath(
  track: LocalMediaTrack | RemoteMediaTrack,
  direction: CallStatDirection,
): Promise<AudioMediaPathResult | null> {
  // `send` is the local mic, `recv` is a remote participant's mic; the
  // publicationSid is unique per track, scoped by direction so a stale key from
  // a prior call never collides. Same `local:`/`remote:` scheme as the
  // diagnostics-dialog poller, so the two are easy to correlate.
  const key =
    direction === "send"
      ? `local:${track.source}:${track.publicationSid}`
      : `remote:${track.participantIdentity}:${track.source}:${track.publicationSid}`;
  try {
    const report = await track.track.getRTCStatsReport();
    if (!report) return null;
    const { summary, sample } = summarizeAudioStats(report, direction, audioMediaPathSamples.get(key));
    const band = audioBitrateBand(summary.bitrateKbps);
    const snapshot: CallMediaPathSnapshot | null =
      band === null
        ? null
        : {
            direction,
            source: "microphone",
            codec: summary.codec,
            iceCandidateType: summary.iceCandidateType,
            iceTransport: summary.iceTransport,
            audioBitrateBand: band,
            videoResolutionBand: null, // audio paths carry no resolution band
          };
    return {
      snapshot,
      key,
      sample,
      rttMs: summary.rttMs,
      packetLossPct: summary.packetLossPct,
    };
  } catch {
    return null;
  }
}

/**
 * Read the call's ICE state once and beacon it. Any published or subscribed
 * track's stats report carries the transport / candidate-pair / local-candidate
 * entries for the peer connection, so the first available track is enough — the
 * ICE path is a property of the connection, not of a track.
 * `getRTCStatsReport()` rejects mid-teardown, so a failure drops this pass.
 */
async function sampleIcePath(): Promise<unknown[] | null> {
  const track = localTracks.value[0] ?? remoteTracks.value[0];
  if (!track) return null;
  try {
    const report = await track.track.getRTCStatsReport();
    if (!report) return null;
    return iceStatsEntries(report);
  } catch {
    // Telemetry must never be the reason a call misbehaves.
    return null;
  }
}

/** Sample every published/subscribed video track once and beacon the distinct
 * media paths. Skips re-entrant ticks so a slow pass cannot stack, and discards
 * its results if the call ended (or restarted) while `getStats()` was awaited. */
async function sampleMediaPaths(generation: number): Promise<void> {
  if (mediaPathSampling) return;
  mediaPathSampling = true;
  try {
    const [iceEntries, videoSnapshots, audioResults] = await Promise.all([
      sampleIcePath(),
      Promise.all([
        ...localTracks.value
          .filter((track) => track.kind === "video")
          .map((track) => sampleTrackMediaPath(track, "send")),
        ...remoteTracks.value
          .filter((track) => track.kind === "video")
          .map((track) => sampleTrackMediaPath(track, "recv")),
      ]),
      // Only the microphone carries the voice-clarity lever; screen-share audio
      // is system/tab audio, not speech, so it is excluded.
      Promise.all([
        ...localTracks.value
          .filter((track) => track.source === "microphone")
          .map((track) => sampleAudioTrackMediaPath(track, "send")),
        ...remoteTracks.value
          .filter((track) => track.source === "microphone")
          .map((track) => sampleAudioTrackMediaPath(track, "recv")),
      ]),
    ]);
    // Bail if polling was stopped/restarted while we awaited: this pass belongs
    // to a torn-down call and must not observe into the (reset) beacon or seed
    // the next call's bitrate samples.
    if (generation !== mediaPathGeneration) return;
    // Observed only after the generation check: a pass that outlived the
    // call must not poison the (reset) ICE beacon for the next call.
    if (iceEntries) callIceBeacon.observe(iceEntries);
    for (const result of videoSnapshots) {
      if (!result) continue;
      observeCallStats({ rttMs: result.rttMs, packetLossPct: result.packetLossPct });
      if (result.snapshot) callMediaPathBeacon.observe(result.snapshot);
    }
    for (const result of audioResults) {
      if (!result) continue;
      observeCallStats({ rttMs: result.rttMs, packetLossPct: result.packetLossPct });
      if (result.sample) audioMediaPathSamples.set(result.key, result.sample);
      if (result.snapshot) callMediaPathBeacon.observe(result.snapshot);
    }
  } finally {
    mediaPathSampling = false;
  }
}

function startMediaPathPolling(): void {
  stopMediaPathPolling();
  const generation = mediaPathGeneration;
  // Sample immediately so a short call (or the early-settling host/udp path)
  // still beacons, then poll for any mid-call re-route.
  void sampleMediaPaths(generation);
  mediaPathTimer = setInterval(() => void sampleMediaPaths(generation), MEDIA_PATH_SAMPLE_INTERVAL_MS);
}

function stopMediaPathPolling(): void {
  // Invalidate any in-flight tick so its post-await observe no-ops.
  mediaPathGeneration += 1;
  if (mediaPathTimer) {
    clearInterval(mediaPathTimer);
    mediaPathTimer = null;
  }
  // Drop the per-track bitrate samples so the next call's first delta is not
  // diffed against a previous call's byte counter.
  audioMediaPathSamples.clear();
}

export function useCallEngine(): {
  engine: CallEngine;
  remoteTracks: Ref<RemoteMediaTrack[]>;
  localTracks: Ref<LocalMediaTrack[]>;
  activeSpeakerIdentities: Ref<ReadonlySet<string>>;
  promotedSpeakerIdentity: Ref<string | null>;
} {
  if (!singletonEngine) {
    singletonEngine = new CallEngine();
    singletonEngine.on("trackSubscribed", (track) => {
      remoteTracks.value = [...remoteTracks.value, track];
    });
    singletonEngine.on("trackUnsubscribed", (track) => {
      remoteTracks.value = remoteTracks.value.filter(
        (existing) => existing.publicationSid !== track.publicationSid,
      );
    });
    singletonEngine.on("localTrackPublished", (track) => {
      localTracks.value = [...localTracks.value, track];
      if (track.source === "screen_share") syncScreenShareEnabled(true);
    });
    singletonEngine.on("localTrackUnpublished", (track) => {
      localTracks.value = localTracks.value.filter(
        (existing) => existing.publicationSid !== track.publicationSid,
      );
      if (track.source === "screen_share") syncScreenShareEnabled(false);
    });
    singletonEngine.on("connected", ({ roomName }) => {
      // Adopt the #1452 correlation id before any call-scoped beacon can
      // fire, so every event of this call joins with the server's
      // call-setup logs and the LiveKit webhook logs. Fire-and-forget:
      // the digest is a microtask, and an event that races it simply
      // carries `unknown` rather than blocking the connect path.
      void adoptCallCorrelationId(roomName);
    });
    singletonEngine.on("connected", ({ localIdentity, remoteIdentities }) => {
      // Seed the LK-truth projection for the active MUC room with
      // the initial remote-participant snapshot LK gave us at
      // connect time. Local identity is included so the room view
      // shows ourselves alongside peers without waiting for the
      // (already-true) Muji presence to also broadcast.
      const roomJid = activeMucRoomJid();
      if (!roomJid) return;
      setLiveCallParticipants(roomJid, [localIdentity, ...remoteIdentities]);
    });
    singletonEngine.on("connected", () => {
      // Start the observational media-path poll for the whole call (every
      // call, not just when the diagnostics dialog is open). Separate from the
      // MUC-projection handler above, which early-returns for 1:1 DM calls.
      startMediaPathPolling();
    });
    singletonEngine.on("connected", () => {
      // Stamp the call clock so the stage-header's elapsed timer starts at the
      // moment the LiveKit room actually connects (not at the optimistic
      // `active` transition). Reset on `disconnected`, mirroring the
      // connection-quality lifecycle.
      setCallActiveSince(Date.now());
    });
    const engineForDeviceChanges = singletonEngine;
    singletonEngine.on("connected", () => {
      // Keep a call-lifetime `devicechange` listener active even when the
      // settings dialog is closed, so a hot-unplugged in-use mic/camera can
      // surface a notice and fall back to the browser default immediately.
      startCallDeviceChangeListener(engineForDeviceChanges);
    });
    singletonEngine.on("mediaDevicesError", ({ source, error }) => {
      // A best-effort mic/cam capture failed (no device / denied
      // permission). The call stays connected as receive-only; record
      // the issue so the non-blocking notice can explain it and offer
      // a retry. NOT routed through reportCallError — the persistent
      // notice is the single channel, avoiding a double-surfaced error.
      recordMediaIssue(source === "audio" ? "mic" : "cam", error);
    });
    singletonEngine.on("audioPlaybackStatusChanged", (canPlaybackAudio) => {
      setCallAudioPlaybackBlocked(!canPlaybackAudio);
    });
    singletonEngine.on("micAudioProcessingChanged", (state) => {
      // Verified (applied) browser-native audio processing of the local
      // mic — drives the read-only indicator in the call-settings dialog.
      setMicAudioProcessing(state);
    });
    singletonEngine.on("aiNoiseFilterChanged", (state) => {
      // Verified AI noise filter (read from the attached processor) — drives
      // the AI-filter row in the settings dialog. A model that is genuinely
      // running clears any prior attach-failure notice.
      setMicAiNoiseFilter(state);
      if (state.kind === "active" && state.model !== null) clearAiNoiseFilterError();
    });
    singletonEngine.on("verifiedMicProcessingChanged", (snapshot) => {
      // One consistent {browser-trio, AI-model} snapshot per settled change —
      // beacon it for fleet measurement (#913/#914). The beacon de-dupes, so
      // redundant recomputes collapse to one event per distinct state.
      micAudioProcessingBeacon.observe(snapshot);
    });
    singletonEngine.on("aiNoiseFilterError", ({ model }) => {
      // Non-blocking: the engine failed open to the raw mic. Surface which
      // model couldn't start so the dialog can explain it.
      setAiNoiseFilterError(model);
    });
    singletonEngine.on("backgroundEffectChanged", (state) => {
      // Verified camera background effect (read from the attached processor) —
      // drives the Background section in the settings dialog. An effect that is
      // genuinely running clears any prior attach-failure notice.
      setCameraBackground(state);
      if (state.kind === "active" && state.effect.kind !== "off") clearBackgroundEffectError();
    });
    singletonEngine.on("backgroundEffectError", ({ effect }) => {
      // Non-blocking: the engine failed open to the raw camera. Surface which
      // effect couldn't start so the dialog can explain it.
      setBackgroundEffectError(effect);
    });
    singletonEngine.on("connectionQualityChanged", (quality) => {
      // LiveKit re-scored the local participant's connection — drives the
      // ambient signal-bars chip on the call bar.
      setCallConnectionQuality(quality);
      observeCallConnectionQuality(quality);
    });
    singletonEngine.on("connectionPhaseChanged", (phase) => {
      // Transport phase (connected ↔ reconnecting) — overrides the quality
      // bars with a "Reconnecting…" label while the path is re-establishing.
      setCallConnectionPhase(phase);
      observeCallConnectionPhase(phase);
    });
    singletonEngine.on("transportReconnecting", () => {
      // A full media-transport reconnect is the client-observable ICE
      // restart (#1452). The engine deliberately does not fire this for
      // SignalReconnecting (signaling WebSocket recovery, media intact),
      // so signaling blips never inflate the ICE-restart SLI.
      callIceBeacon.noteIceRestart();
    });
    singletonEngine.on("iceCredentialsRefreshed", () => {
      callIceBeacon.noteCredentials("refreshed");
    });
    singletonEngine.on("iceCredentialsExpired", () => {
      callIceBeacon.noteCredentials("expired");
    });
    singletonEngine.on("activeSpeakersChanged", (identities) => {
      // LiveKit re-derived who is speaking. Feed it through the brief hold so
      // the Gallery highlight does not flicker on transient sounds.
      lastSpeakingIdentities = identities;
      pumpActiveSpeakers();
    });
    singletonEngine.on("participantConnected", (identity) => {
      const roomJid = activeMucRoomJid();
      if (!roomJid) return;
      addLiveCallParticipant(roomJid, identity);
    });
    singletonEngine.on("participantDisconnected", (identity) => {
      // Clear any active-speaker highlight + Speaker-view promotion the leaver
      // held, so neither survives their departure to re-apply on a fast rejoin.
      forgetDepartedSpeaker(identity);
      const roomJid = activeMucRoomJid();
      if (!roomJid) return;
      removeLiveCallParticipant(roomJid, identity);
    });
    singletonEngine.on("disconnected", ({ origin, reason }) => {
      const disconnectedCall = $callState.get();
      const disconnectedMucSlot = activeMucCallSlot();
      if (origin !== "local" && disconnectedCall.phase === "active") {
        const terminal = finishCallAttemptForTransportDisconnect(disconnectedCall.sid);
        clearCallState({ endReason: terminal?.endReason ?? "error" });
        reportCallError(new Error(transportDisconnectMessage(reason)));
      }
      remoteTracks.value = [];
      localTracks.value = [];
      // Drop the active-speaker highlight + its pending sweep so a stale
      // speaker from the call that just ended can't bleed into the next one.
      resetActiveSpeakers();
      syncScreenShareEnabled(false);
      // Drop any "joined without mic/camera" notice — it belongs to the
      // call that just ended, not the next one.
      clearAllMediaIssues();
      setCallAudioPlaybackBlocked(false);
      // The mic-processing readout belonged to the call that just
      // ended; reset so the next call's dialog starts from `no-mic`.
      resetMicAudioProcessing();
      // Same for the verified AI-filter readout and any attach-failure notice.
      resetMicAiNoiseFilter();
      clearAiNoiseFilterError();
      // Same for the verified camera-background readout and its notice.
      resetCameraBackground();
      clearBackgroundEffectError();
      // Re-arm the fleet-measurement beacon so the next call emits its
      // first verified state even if it matches the last call's.
      micAudioProcessingBeacon.reset();
      // Stop the media-path poll and re-arm its beacon for the next call.
      stopMediaPathPolling();
      stopCallDeviceChangeListener();
      callMediaPathBeacon.reset();
      // Same for the ICE beacon: drop this call's seen-set and restart
      // count so the next call measures its own connectivity.
      callIceBeacon.reset();
      // Drop the correlation id so a stray post-call event cannot be
      // attributed to the call that just ended.
      clearCallCorrelationId();
      // Drop the connection-quality chip so a stale `poor`/`reconnecting`
      // from the call that just ended can't bleed into the next call.
      resetCallConnectionQuality();
      // Stop the elapsed-timer clock so the next call's header starts from
      // "Connecting…" instead of inheriting this call's start instant.
      resetCallActiveSince();
      // The active call's room — if any — is no longer in our LK
      // view. Drop the LK snapshot so the room's UI reverts to the
      // Muji-derived (server-bridged) view, which the LK webhook
      // bridge keeps honest within seconds.
      const slot = disconnectedMucSlot;
      if (!slot) return;
      // Suppress our own stale Muji nick for one render cycle: the
      // server-authoritative `<muji/>` absence broadcast arrives ~1
      // RTT after LK fires `disconnected`, so without this marker the
      // resolved participant list would fall back to a Muji view that
      // still lists us — bouncing the indicator 0 → N → 0.
      markRoomLeavingCall(slot.roomJid, slot.selfNick);
      clearLiveCallParticipants(slot.roomJid);
    });
  }
  return {
    engine: singletonEngine,
    remoteTracks,
    localTracks,
    activeSpeakerIdentities,
    promotedSpeakerIdentity,
  };
}

/**
 * Read the bare room JID of the MUC call our `$callState` slot is
 * currently bound to, or `null` when no MUC call is active. The LK
 * projection key on this so a 1:1 DM call's engine events do not
 * leak into the MUC participant store.
 */
function activeMucRoomJid(): string | null {
  return activeMucCallSlot()?.roomJid ?? null;
}

/**
 * Read the active MUC call's room JID together with our own MUC nick,
 * or `null` when no MUC call is bound to the slot. The nick lets the
 * teardown path suppress our own stale Muji presence until the server
 * broadcasts our `<muji/>` absence.
 */
function activeMucCallSlot(): { roomJid: string; selfNick: string | null } | null {
  const state = $callState.get();
  if (state.phase !== "active" && state.phase !== "muc-pending") return null;
  if (state.kind !== "muc") return null;
  return { roomJid: state.peer, selfNick: state.selfNick ?? null };
}
