import {
  ConnectionQuality,
  ConnectionState,
  Room,
  RoomEvent,
  Track,
  VideoPresets,
  type LocalParticipant,
  type LocalTrack,
  type LocalTrackPublication,
  type Participant,
  type RemoteParticipant,
  type RemoteTrack,
  type RemoteTrackPublication,
  type TrackPublication,
  type TrackPublishOptions,
  type VideoCaptureOptions,
} from "livekit-client";
import { videoPublishPlan } from "./video-codec/video-publish";
import {
  currentVideoCodecSupportEnv,
  videoCodecSupport,
  type VideoCodecSupport,
} from "./video-codec/support";
import { currentScreenCaptureEnv } from "./video-codec/screen-capture-env";
import type { LiveKitJoin } from "./types";
import type { CallConnectionPhase, CallConnectionQuality } from "./connection-quality";
import {
  $devicePrefs,
  audioProcessingConstraints,
  type AudioProcessingConstraints,
  type AudioProcessingPrefs,
} from "./device-prefs";
import { validateLiveKitGrant } from "./dm-call-activity";
import { activeMicAudioProcessing, type MicAudioProcessing } from "./mic-audio-processing";
import { effectiveAudioProcessing } from "./ai-noise-filter/effective-audio-processing";
import { activeAiNoiseFilter, type AiNoiseFilterState } from "./ai-noise-filter/mic-ai-noise-filter";
import type { NoiseModelId } from "./ai-noise-filter/model-id";
import type { AudioNoiseProcessor } from "./ai-noise-filter/processor";
import { makeNoiseProcessor } from "./ai-noise-filter/registry";
import { runAiNoiseFilterReconcile, type ProcessorTarget } from "./ai-noise-filter/reconcile";
import type { VerifiedCallAudioProcessing } from "./call-audio-processing-telemetry";

/**
 * The local mic track surface the AI-noise reconciler drives — the subset of
 * `LocalAudioTrack` it needs. Structural so the engine can build a
 * `ProcessorTarget` from a real track and tests can inject a fake.
 */
type ProcessorCapableTrack = {
  getProcessor(): { name: string } | undefined;
  setProcessor(processor: AudioNoiseProcessor): Promise<void>;
  stopProcessor(): Promise<void>;
};

/**
 * Camera capture ceiling. AdaptiveStream (`adaptiveStream: true`) and
 * simulcast still scale each subscriber DOWN to the layer matching its
 * rendered tile, so this only governs the maximum a large/expanded tile
 * can pull — it is not the bandwidth every viewer pays. h1080 lifts the
 * per-publisher upstream ceiling to ~3 Mbps (vs. 720p's 1.7 Mbps).
 */
const CAMERA_CAPTURE_RESOLUTION = VideoPresets.h1080.resolution;

export function audioCaptureDefaultsForPrefs(prefs: {
  mic: string | null;
  audioProcessing: AudioProcessingPrefs;
}): AudioProcessingConstraints & { deviceId?: string } {
  return {
    ...audioProcessingConstraints(prefs.audioProcessing),
    deviceId: prefs.mic ?? undefined,
  };
}

export function callRoomOptionsForPrefs(prefs: {
  mic: string | null;
  cam: string | null;
  audioProcessing: AudioProcessingPrefs;
}) {
  return {
    adaptiveStream: true,
    dynacast: true,
    webAudioMix: true,
    // Initial capture uses the user's stored constraints. If a model is the
    // persisted choice, the browser NS is flipped off reactively once the
    // model actually attaches (see syncEffectiveCaptureConstraints) — so a
    // model that fails to load never leaves capture with NS force-disabled.
    audioCaptureDefaults: audioCaptureDefaultsForPrefs(prefs),
    videoCaptureDefaults: {
      deviceId: prefs.cam ?? undefined,
      resolution: CAMERA_CAPTURE_RESOLUTION,
    },
    // Screen-share encoding/codec is set per-publish by `videoPublishPlan`
    // (it is capability-dependent), so no global `publishDefaults` here.
  };
}

/**
 * Identifies a remote media track surfaced to the UI. The `kind`
 * mirrors LiveKit's `Track.Kind` so the renderer can pick a
 * `<video>` vs `<audio>` element without re-deriving it.
 */
export type RemoteMediaTrack = {
  participantIdentity: string;
  publicationSid: string;
  kind: "audio" | "video";
  source: CallTrackSource;
  track: RemoteTrack;
};

/**
 * Identifies a local media track (the user's own camera / mic /
 * screenshare) so the overlay can render a self-preview tile next to
 * the remote ones. Symmetric with [`RemoteMediaTrack`] so the renderer
 * can branch on `kind` alone.
 *
 * `participantIdentity` is the LiveKit identity the SFU minted the
 * JWT for — typically the full XMPP JID of the device that started
 * the call.
 */
export type LocalMediaTrack = {
  participantIdentity: string;
  publicationSid: string;
  kind: "audio" | "video";
  source: CallTrackSource;
  track: LocalTrack;
};

export type CallTrackSource =
  | "camera"
  | "microphone"
  | "screen_share"
  | "screen_share_audio";

type CallAudioTrackSource = Extract<CallTrackSource, "microphone" | "screen_share_audio">;

export type ParticipantAudioVolumeStore = Record<string, number>;

export type ParticipantAudioVolumeIdentity = {
  participantIdentity: string;
  source: CallAudioTrackSource;
};

type VolumeAdjustableTrack = {
  setVolume(volume: number): unknown;
};

function participantAudioVolumeKey(
  identity: ParticipantAudioVolumeIdentity,
): string {
  return `${identity.participantIdentity}:${identity.source}`;
}

export function resolveParticipantAudioVolume(
  store: ParticipantAudioVolumeStore,
  identity: ParticipantAudioVolumeIdentity,
): number {
  return normalizeParticipantAudioVolume(store[participantAudioVolumeKey(identity)]);
}

export type CallEngineEvents = {
  trackSubscribed: (track: RemoteMediaTrack) => void;
  trackUnsubscribed: (track: RemoteMediaTrack) => void;
  /**
   * Fires when the local participant publishes a track (camera on,
   * mic on, screenshare, …). Gives the overlay a reactive hook for
   * the self-preview tile — strictly better than the one-shot
   * `attachLocalCamera` snapshot it superseded.
   */
  localTrackPublished: (track: LocalMediaTrack) => void;
  /**
   * Fires when the local participant unpublishes (camera off, mic
   * mute via unpublish path, etc).
   */
  localTrackUnpublished: (track: LocalMediaTrack) => void;
  /**
   * Fires when a remote participant transitions to LiveKit's `active`
   * state. Source of truth for "this peer is in the call" for the
   * `$mucCallParticipants` projection in `use-call-engine.ts`.
   */
  participantConnected: (identity: string) => void;
  participantDisconnected: (identity: string) => void;
  /**
   * Fires once `connect()` resolves and the local participant has
   * reached the `active` state in LiveKit. Surfaces the initial
   * remote-participant set so the projection can seed
   * `$mucCallParticipants` without missing peers that connected
   * before our event listener attached.
   */
  connected: (snapshot: { localIdentity: string; remoteIdentities: string[] }) => void;
  disconnected: (reason?: string) => void;
  /**
   * Fires when LiveKit reports whether remote audio can currently
   * play. Browser autoplay policy can suspend Web Audio playback until
   * a user gesture calls `startAudio()`.
   */
  audioPlaybackStatusChanged: (canPlaybackAudio: boolean) => void;
  /**
   * Fires when a best-effort capture (mic / cam enable) fails —
   * typically a missing device (`NotFoundError`) or a denied
   * permission (`NotAllowedError`). NON-FATAL: the room stays
   * connected and the user remains a receive-only participant, so the
   * UI uses this to show a "joined without microphone/camera" notice
   * rather than ending the call. Mirrors LiveKit's
   * `RoomEvent.MediaDevicesError` but carries which capture failed.
   */
  mediaDevicesError: (info: { source: "audio" | "video"; error: unknown }) => void;
  /**
   * Fires when the *applied* browser-native audio processing of the
   * local mic changes — on mic publish, unpublish, or a mid-call mic
   * device switch. Carries the verified state (read from the live
   * track's `getSettings()`), NOT the requested constraints, so the UI
   * can show whether noise suppression / echo cancellation / auto gain
   * were actually honored. See `mic-audio-processing.ts`.
   */
  micAudioProcessingChanged: (state: MicAudioProcessing) => void;
  /**
   * Fires when the *verified* AI noise filter on the local mic changes — on
   * mic publish/unpublish, a device switch, mute/unmute, or an explicit model
   * selection. Carries the model read back from the attached processor's name
   * (`getProcessor()`), NOT merely the requested pref, so the indicator is
   * honest even when an attach silently fails. The #911 `getSettings()` read
   * cannot see a processor, hence this sibling signal. See `mic-ai-noise-filter.ts`.
   */
  aiNoiseFilterChanged: (state: AiNoiseFilterState) => void;
  /**
   * Fires when a model fails to attach (wasm/model fetch error, unsupported
   * device, missing backend). NON-FATAL: the engine fails open to the raw mic
   * and the UI shows a non-blocking notice; the per-(track,model) guard stops
   * the defensive re-runs from retrying until a fresh track or re-selection.
   */
  aiNoiseFilterError: (info: { model: NoiseModelId; error: unknown }) => void;
  /**
   * Fires once per settled mic-processing change with BOTH layers (browser
   * constraint trio + verified AI model) computed together, so the fleet
   * beacon always sees a consistent snapshot. Telemetry-only; the UI reads
   * `micAudioProcessingChanged` / `aiNoiseFilterChanged` for its two stores.
   */
  verifiedMicProcessingChanged: (snapshot: VerifiedCallAudioProcessing) => void;
  /**
   * Fires when LiveKit re-scores the LOCAL participant's connection
   * quality (`excellent` → `lost`). Remote participants' quality is
   * intentionally not surfaced — the indicator is self-only. Push-based:
   * no polling, no `getStats`.
   */
  connectionQualityChanged: (quality: CallConnectionQuality) => void;
  /**
   * Fires when the room transport phase changes (connected ↔
   * reconnecting ↔ disconnected). Drives the "Reconnecting…" label,
   * which must override the last quality sample while the path is down.
   */
  connectionPhaseChanged: (phase: CallConnectionPhase) => void;
};

/**
 * Thin, single-call wrapper around `livekit-client`'s `Room`.
 *
 * One engine instance maps to one Jingle session: `connect()` is
 * driven by the `LiveKitJoin` issued by the server in the rewritten
 * Waddle transport, and `disconnect()` is paired with
 * `session-terminate`. The Vue layer subscribes to engine events to
 * render local + remote video tiles.
 */
export class CallEngine {
  private room: Room | null = null;
  private participantAudioVolumes: ParticipantAudioVolumeStore = {};
  private subscribedAudioTracks = new Map<string, Set<VolumeAdjustableTrack>>();
  private listeners: { [K in keyof CallEngineEvents]: Set<CallEngineEvents[K]> } = {
    trackSubscribed: new Set(),
    trackUnsubscribed: new Set(),
    localTrackPublished: new Set(),
    localTrackUnpublished: new Set(),
    participantConnected: new Set(),
    participantDisconnected: new Set(),
    connected: new Set(),
    disconnected: new Set(),
    audioPlaybackStatusChanged: new Set(),
    mediaDevicesError: new Set(),
    micAudioProcessingChanged: new Set(),
    aiNoiseFilterChanged: new Set(),
    aiNoiseFilterError: new Set(),
    verifiedMicProcessingChanged: new Set(),
    connectionQualityChanged: new Set(),
    connectionPhaseChanged: new Set(),
  };

  /**
   * The user's selected AI noise model (or null/off), seeded from prefs at
   * connect. Source of "what should be attached" for the reconciler.
   */
  private desiredAiNoiseModel: NoiseModelId | null = null;
  /**
   * Models that have failed to attach to the CURRENT mic track. Skips a retry
   * on every defensive re-run; cleared on a fresh publish or a re-selection.
   */
  private aiNoiseFailedModels = new Set<NoiseModelId>();
  /** Serializes reconciles so bursty triggers can't race on setProcessor. */
  private aiReconcileChain: Promise<void> = Promise.resolve();
  /**
   * Whether the live capture currently has browser NS forced off for an
   * attached model. Tracks the *applied* effective constraint so
   * `syncEffectiveCaptureConstraints` only restarts capture when it flips.
   */
  private appliedModelActiveForCapture = false;
  /** Builds a processor for a model; injectable so tests avoid real wasm. */
  private readonly makeAiNoiseProcessor: (model: NoiseModelId) => Promise<AudioNoiseProcessor>;
  /**
   * Which video codecs this device can publish, probed once at construction.
   * Injectable so tests assert the forwarded options for capable vs.
   * incapable devices without real WebRTC.
   */
  private readonly codecSupport: VideoCodecSupport;

  constructor(opts?: {
    makeAiNoiseProcessor?: (model: NoiseModelId) => Promise<AudioNoiseProcessor>;
    videoCodecSupport?: VideoCodecSupport;
  }) {
    this.makeAiNoiseProcessor = opts?.makeAiNoiseProcessor ?? makeNoiseProcessor;
    this.codecSupport = opts?.videoCodecSupport ?? videoCodecSupport(currentVideoCodecSupportEnv());
  }

  /** LiveKit identity of the local participant, populated once
   *  `connect()` resolves. Exposed so the overlay can render the
   *  self-preview tile with the same label as remote tiles use. */
  get localIdentity(): string | null {
    return this.room?.localParticipant.identity ?? null;
  }

  /**
   * Whether the local mic / camera track is ACTUALLY published right
   * now, read from LiveKit's own source of truth. Used to seed the
   * mic/cam toggle UI to reality after the best-effort initial capture
   * (which may have silently failed — no device / denied permission);
   * the UI must never assume the requested media actually published.
   */
  get micEnabled(): boolean {
    return this.room?.localParticipant.isMicrophoneEnabled ?? false;
  }

  get cameraEnabled(): boolean {
    return this.room?.localParticipant.isCameraEnabled ?? false;
  }

  get screenShareEnabled(): boolean {
    return this.room?.localParticipant.isScreenShareEnabled ?? false;
  }

  get canPlaybackAudio(): boolean {
    return this.room?.canPlaybackAudio ?? true;
  }

  async connect(join: LiveKitJoin, opts: { audio: boolean; video: boolean }): Promise<void> {
    if (this.room) throw new Error("CallEngine already connected");
    // Defensive pre-flight: confirm the JWT actually carries a usable
    // `video` grant (roomJoin + a room) before the SDK opens its
    // WebSocket. A mismatched / misconfigured token otherwise fails
    // deep inside `room.connect` with an opaque rejection; this turns
    // it into a clear, actionable local error.
    const grant = validateLiveKitGrant(join.token);
    if (!grant.ok) {
      throw new Error(`Invalid LiveKit join token: ${grant.reason}`);
    }
    const prefs = $devicePrefs.get();
    // Seed the AI-filter desired model from prefs so it re-applies on this
    // call's mic publish; start each call with a clean failure guard and
    // capture-NS state (initial capture uses the user's stored constraints).
    this.desiredAiNoiseModel = prefs.aiNoiseModel;
    this.aiNoiseFailedModels.clear();
    this.appliedModelActiveForCapture = false;
    const room = new Room(callRoomOptionsForPrefs(prefs));
    room.on(RoomEvent.TrackSubscribed, this.handleTrackSubscribed);
    room.on(RoomEvent.TrackUnsubscribed, this.handleTrackUnsubscribed);
    room.on(RoomEvent.LocalTrackPublished, this.handleLocalTrackPublished);
    room.on(RoomEvent.LocalTrackUnpublished, this.handleLocalTrackUnpublished);
    room.on(RoomEvent.ParticipantConnected, this.handleParticipantConnected);
    room.on(RoomEvent.ParticipantDisconnected, this.handleParticipantDisconnected);
    room.on(RoomEvent.Disconnected, this.handleDisconnected);
    room.on(RoomEvent.AudioPlaybackStatusChanged, this.handleAudioPlaybackStatusChanged);
    room.on(RoomEvent.ActiveDeviceChanged, this.handleActiveDeviceChanged);
    room.on(RoomEvent.TrackMuted, this.handleTrackMuteChanged);
    room.on(RoomEvent.TrackUnmuted, this.handleTrackMuteChanged);
    room.on(RoomEvent.ConnectionQualityChanged, this.handleConnectionQualityChanged);
    room.on(RoomEvent.ConnectionStateChanged, this.handleConnectionStateChanged);
    try {
      await room.connect(join.url, join.token);
    } catch (err) {
      // Connect itself failed; the listeners we bound above would
      // otherwise dangle on a `Room` that nothing references but the
      // closure inside the listener — strip them so a future GC has
      // a clean path. `this.room` was never set so the caller stays
      // on the well-defined "not connected" path.
      room.removeAllListeners();
      throw err;
    }
    this.room = room;
    // Emit the initial participant snapshot so subscribers can seed
    // their projection without missing peers that connected before
    // our listeners attached.
    this.emit("connected", {
      localIdentity: room.localParticipant.identity,
      remoteIdentities: Array.from(room.remoteParticipants.values()).map((p) => p.identity),
    });
    // Publishing is BEST-EFFORT and decoupled from joining. `room.connect`
    // above already succeeded, so the user is a fully working receive-only
    // (listen/watch) participant — a missing device or a denied
    // `getUserMedia` permission MUST NOT eject them from the call. Each
    // track is enabled independently (a broken camera can't suppress a
    // working mic) and failures are surfaced via `mediaDevicesError` so the
    // UI can show a non-blocking "joined without mic/camera" notice and
    // offer a retry, instead of tearing the half-joined call down.
    await enableRequestedCapture(
      room.localParticipant,
      { audio: opts.audio, video: opts.video },
      (source, error) => this.emit("mediaDevicesError", { source, error }),
      videoPublishPlan({ source: "camera", capability: this.codecSupport }).publish,
    );
    // Re-apply the speaker preference now that the room has remote
    // audio elements to retarget; the engine ignores it on connect
    // because no `<audio>` exists yet, but every subsequent
    // RoomEvent.TrackSubscribed wires through this.applySpeaker.
    if (prefs.speaker) {
      await this.applySpeakerDevice(prefs.speaker).catch(() => undefined);
    }
  }

  async setMicEnabled(enabled: boolean): Promise<void> {
    if (!this.room) return;
    await this.room.localParticipant.setMicrophoneEnabled(enabled);
  }

  async setCameraEnabled(enabled: boolean): Promise<void> {
    if (!this.room) return;
    const { publish } = videoPublishPlan({ source: "camera", capability: this.codecSupport });
    await this.room.localParticipant.setCameraEnabled(enabled, undefined, publish);
  }

  async setScreenShareEnabled(
    enabled: boolean,
    opts: { audio: boolean },
  ): Promise<void> {
    if (!this.room) return;
    const { capture, publish } = videoPublishPlan({
      source: "screen",
      capability: this.codecSupport,
      screenCapture: currentScreenCaptureEnv(),
    });
    const captureOptions = { ...capture, audio: opts.audio };
    try {
      await this.room.localParticipant.setScreenShareEnabled(enabled, captureOptions, publish);
    } catch (error) {
      if (!enabled || !opts.audio || !isUnsupportedScreenAudioError(error)) throw error;
      await this.room.localParticipant.setScreenShareEnabled(
        true,
        { ...captureOptions, audio: false },
        publish,
      );
    }
  }

  /**
   * Switch the active microphone device mid-call. LiveKit's
   * `switchActiveDevice('audioinput', id)` rewrites the existing
   * publication in place — no re-publish, no Jingle round-trip — so
   * the change is invisible to the peer.
   */
  async setMicDevice(deviceId: string): Promise<void> {
    if (!this.room) return;
    await this.room.switchActiveDevice("audioinput", deviceId);
  }

  /**
   * Restart the live microphone capture with updated browser-native
   * audio-processing constraints. LiveKit swaps the capture track inside
   * the existing publication, so the call stays joined and no Jingle
   * signaling is needed. The effective constraints account for an active AI
   * model superseding the browser `noiseSuppression`.
   */
  async setAudioProcessing(prefs: AudioProcessingPrefs): Promise<void> {
    await this.restartMicCapture(prefs);
    this.settleVerifiedMicProcessing();
  }

  private async restartMicCapture(prefs: AudioProcessingPrefs): Promise<void> {
    const publication = this.room?.localParticipant.getTrackPublication(Track.Source.Microphone);
    if (!publication || publication.isMuted) return;
    const track = publication.track;
    if (!track) return;
    const restartTrack = track?.restartTrack;
    if (typeof restartTrack !== "function") return;
    const settings = track.mediaStreamTrack?.getSettings();
    await restartTrack.call(
      track,
      audioCaptureDefaultsForPrefs({
        mic: settings?.deviceId ?? $devicePrefs.get().mic,
        // Keyed on the VERIFIED attached model, not the desired one: if a
        // selection failed to attach, the browser NS is left exactly as the
        // user set it — never force-disabled with no filter to replace it.
        audioProcessing: effectiveAudioProcessing(prefs, this.verifiedAiNoiseModel()),
      }),
    );
    this.emitMicAudioProcessing();
  }

  /**
   * Select the AI noise model (or null/off). Updates the desired model, gives
   * the new selection a fresh attempt (clears the per-track failure guard),
   * then reconciles the attached processor — which also brings the effective
   * browser NS constraint in line with whatever actually attached. Persistence
   * is the caller's job (the device-selection layer writes the pref).
   */
  async setAiNoiseModel(model: NoiseModelId | null): Promise<void> {
    this.desiredAiNoiseModel = model;
    this.aiNoiseFailedModels.clear();
    await this.ensureAiNoiseFilter();
  }

  /** The model actually attached to the live mic (verified), or null. */
  private verifiedAiNoiseModel(): NoiseModelId | null {
    const state = this.computeAiNoiseFilter();
    return state.kind === "active" ? state.model : null;
  }

  /**
   * Bring the browser-native capture constraints in line with the VERIFIED
   * model: browser `noiseSuppression` is forced off iff a model is actually
   * attached. Restarts capture only when that active-state flips, to avoid
   * churn. This is what flips NS off after a model attaches (and restores it
   * after one stops or fails), reactively, on every reconcile path — including
   * a model changed while muted, which only takes effect once the mic is live.
   */
  private async syncEffectiveCaptureConstraints(): Promise<void> {
    const modelActive = this.verifiedAiNoiseModel() !== null;
    if (modelActive === this.appliedModelActiveForCapture) return;
    this.appliedModelActiveForCapture = modelActive;
    await this.restartMicCapture($devicePrefs.get().audioProcessing);
  }

  /**
   * The local mic track as a processor target, or null when no mic is live /
   * the mic is muted (nothing to attach to).
   */
  private micProcessorTrack(): ProcessorCapableTrack | null {
    const publication = this.room?.localParticipant.getTrackPublication(Track.Source.Microphone);
    if (!publication || publication.isMuted) return null;
    const track = publication.track as unknown as ProcessorCapableTrack | undefined;
    return track && typeof track.getProcessor === "function" ? track : null;
  }

  /**
   * Reconcile the attached processor toward the desired model. Idempotent and
   * serialized; a no-op when no mic track exists (it re-runs on the next
   * publish). Fails open on attach error and emits a typed `aiNoiseFilterError`.
   */
  private ensureAiNoiseFilter(): Promise<void> {
    this.aiReconcileChain = this.aiReconcileChain
      .then(() => this.runEnsureAiNoiseFilter())
      .catch(() => undefined);
    return this.aiReconcileChain;
  }

  private async runEnsureAiNoiseFilter(): Promise<void> {
    const track = this.micProcessorTrack();
    if (!track) {
      // No live mic to attach to (none published, or muted) — surface the
      // honest `no-mic` state; the desired model re-applies on the next publish.
      this.emitAiNoiseFilter();
      this.settleVerifiedMicProcessing();
      return;
    }
    const target: ProcessorTarget<AudioNoiseProcessor> = {
      currentProcessorName: () => track.getProcessor()?.name,
      attach: (processor) => track.setProcessor(processor),
      clear: () => track.stopProcessor(),
    };
    const outcome = await runAiNoiseFilterReconcile({
      target,
      desired: this.desiredAiNoiseModel,
      makeProcessor: this.makeAiNoiseProcessor,
      failedModels: this.aiNoiseFailedModels,
    });
    if (outcome.action === "failed") {
      this.emit("aiNoiseFilterError", { model: outcome.model, error: outcome.error });
    }
    // Flip browser NS to match what actually attached, then surface the
    // settled verified state for the UI and the fleet beacon.
    await this.syncEffectiveCaptureConstraints();
    this.emitAiNoiseFilter();
    this.settleVerifiedMicProcessing();
  }

  /** Verified AI-filter state read from the live processor's name. */
  private computeAiNoiseFilter(): AiNoiseFilterState {
    const track = this.micProcessorTrack();
    if (!track) return { kind: "no-mic" };
    return activeAiNoiseFilter(track.getProcessor()?.name);
  }

  private emitAiNoiseFilter(): void {
    this.emit("aiNoiseFilterChanged", this.computeAiNoiseFilter());
  }

  /**
   * Emit ONE combined verified snapshot (browser trio + AI model) for the
   * fleet beacon. Computed fresh from the live track so the two layers are
   * always consistent — unlike reading one store while the other lags, which
   * would beacon impossible `{NS on, filter on}` combos and drop the true
   * resting state. Drives only telemetry; the UI reads the two granular stores.
   */
  private settleVerifiedMicProcessing(): void {
    this.emit("verifiedMicProcessingChanged", {
      processing: this.computeMicAudioProcessing(),
      aiNoiseFilter: this.computeAiNoiseFilter(),
    });
  }

  /**
   * Same shape as `setMicDevice` but for the camera device.
   */
  async setCameraDevice(deviceId: string): Promise<void> {
    if (!this.room) return;
    await this.room.switchActiveDevice("videoinput", deviceId);
  }

  /**
   * Route the room's remote audio through a specific output device.
   * Uses LiveKit's built-in `audiooutput` switch for browsers that can
   * route Web Audio output sinks; falls back to a no-op on unsupported
   * browsers.
   */
  async setSpeakerDevice(deviceId: string): Promise<void> {
    await this.applySpeakerDevice(deviceId);
  }

  async startAudio(): Promise<void> {
    await this.room?.startAudio();
  }

  setParticipantAudioVolume(
    identity: ParticipantAudioVolumeIdentity & { volume: number },
  ): void {
    const key = participantAudioVolumeKey(identity);
    const volume = normalizeParticipantAudioVolume(identity.volume);
    this.participantAudioVolumes[key] = volume;
    for (const track of this.subscribedAudioTracks.get(key) ?? []) {
      track.setVolume(volume);
    }
  }

  private async applySpeakerDevice(deviceId: string): Promise<void> {
    if (!this.room) return;
    if (typeof this.room.switchActiveDevice !== "function") return;
    try {
      await this.room.switchActiveDevice("audiooutput", deviceId);
    } catch {
      // Browser doesn't support sinkId; the user already saw the
      // disabled chip in the picker. Swallow silently here.
    }
  }

  async disconnect(): Promise<void> {
    const room = this.room;
    this.room = null;
    if (!room) return;
    this.clearParticipantAudioVolumes();
    // Drop subscribers' track caches synchronously. The LiveKit
    // `Disconnected` event is the natural trigger, but we unregister
    // `handleDisconnected` immediately below so the event never reaches
    // us — without this synthetic emit, `useCallEngine` would carry the
    // last call's tracks into the next session and the overlay would
    // render duplicate tiles.
    this.emit("disconnected", "local");
    room.off(RoomEvent.TrackSubscribed, this.handleTrackSubscribed);
    room.off(RoomEvent.TrackUnsubscribed, this.handleTrackUnsubscribed);
    room.off(RoomEvent.LocalTrackPublished, this.handleLocalTrackPublished);
    room.off(RoomEvent.LocalTrackUnpublished, this.handleLocalTrackUnpublished);
    room.off(RoomEvent.ParticipantConnected, this.handleParticipantConnected);
    room.off(RoomEvent.ParticipantDisconnected, this.handleParticipantDisconnected);
    room.off(RoomEvent.Disconnected, this.handleDisconnected);
    room.off(RoomEvent.AudioPlaybackStatusChanged, this.handleAudioPlaybackStatusChanged);
    room.off(RoomEvent.ActiveDeviceChanged, this.handleActiveDeviceChanged);
    room.off(RoomEvent.TrackMuted, this.handleTrackMuteChanged);
    room.off(RoomEvent.TrackUnmuted, this.handleTrackMuteChanged);
    room.off(RoomEvent.ConnectionQualityChanged, this.handleConnectionQualityChanged);
    room.off(RoomEvent.ConnectionStateChanged, this.handleConnectionStateChanged);
    // Tear the mic noise processor down explicitly first: its destroy() stops a
    // private clone of the capture track that holds the input device open. A
    // normal room.disconnect() stops local tracks (which destroys the processor)
    // for us, but doing it here means an erroring/partial disconnect can't leave
    // the device — and its indicator — live until GC.
    const micTrack = room.localParticipant?.getTrackPublication?.(Track.Source.Microphone)
      ?.track as unknown as ProcessorCapableTrack | undefined;
    if (micTrack && typeof micTrack.getProcessor === "function" && micTrack.getProcessor()) {
      await micTrack.stopProcessor().catch(() => undefined);
    }
    await room.disconnect();
  }

  on<K extends keyof CallEngineEvents>(event: K, cb: CallEngineEvents[K]): () => void {
    this.listeners[event].add(cb);
    return () => {
      this.listeners[event].delete(cb);
    };
  }

  private emit<K extends keyof CallEngineEvents>(event: K, ...args: Parameters<CallEngineEvents[K]>): void {
    for (const cb of this.listeners[event]) {
      (cb as (...a: Parameters<CallEngineEvents[K]>) => void)(...args);
    }
  }

  private handleTrackSubscribed = (
    track: RemoteTrack,
    publication: RemoteTrackPublication,
    participant: RemoteParticipant,
  ) => {
    const kind = mapKind(track.kind);
    if (!kind) return;
    const source = mapTrackSource(publication.source, kind);
    if (isCallAudioTrackSource(source)) {
      this.rememberSubscribedAudioTrack({
        participantIdentity: participant.identity,
        source,
        track,
      });
    }
    this.emit("trackSubscribed", {
      participantIdentity: participant.identity,
      publicationSid: publication.trackSid,
      kind,
      source,
      track,
    });
  };

  private handleTrackUnsubscribed = (
    track: RemoteTrack,
    publication: RemoteTrackPublication,
    participant: RemoteParticipant,
  ) => {
    const kind = mapKind(track.kind);
    if (!kind) return;
    const source = mapTrackSource(publication.source, kind);
    if (isCallAudioTrackSource(source)) {
      this.forgetSubscribedAudioTrack({
        participantIdentity: participant.identity,
        source,
        track,
      });
    }
    this.emit("trackUnsubscribed", {
      participantIdentity: participant.identity,
      publicationSid: publication.trackSid,
      kind,
      source,
      track,
    });
  };

  private handleLocalTrackPublished = (
    publication: LocalTrackPublication,
    participant: LocalParticipant,
  ) => {
    const track = publication.track;
    const kind = mapKind(publication.kind);
    if (!track || !kind) return;
    const source = mapTrackSource(publication.source, kind);
    this.emit("localTrackPublished", {
      participantIdentity: participant.identity,
      publicationSid: publication.trackSid,
      kind,
      source,
      track,
    });
    // The local mic just started capturing: surface its *applied*
    // audio processing now that `getSettings()` reflects the real
    // constraints the browser honored (which can differ from what we
    // requested in `audioCaptureDefaults`).
    if (source === "microphone") {
      // A brand-new mic track: clear the per-track failure guard so a model
      // that failed on a previous track gets one clean attempt here. The new
      // capture starts from the stored constraints (browser NS per pref), so
      // reset the applied-capture flag too — otherwise a stale `true` from the
      // previous track makes `syncEffectiveCaptureConstraints` skip the restart
      // that should force NS off after the model re-attaches (double NS).
      this.aiNoiseFailedModels.clear();
      this.appliedModelActiveForCapture = false;
      this.emitMicAudioProcessing();
      void this.ensureAiNoiseFilter();
    }
  };

  private handleLocalTrackUnpublished = (
    publication: LocalTrackPublication,
    participant: LocalParticipant,
  ) => {
    // Recompute the mic-processing readout FIRST, decoupled from the
    // track-dependent event below. LiveKit can deliver an unpublish whose
    // `publication.track` is already detached (the underlying
    // MediaStreamTrack ended before the event fired); the `!track` guard
    // would then skip the reset and leave the indicator on a stale trio
    // for a mic that is gone. `computeMicAudioProcessing` reads the
    // participant's current mic publication, not this argument, so it is
    // safe without `track` and correctly falls back to `no-mic`.
    if (publication.source === Track.Source.Microphone) {
      this.emitMicAudioProcessing();
      // The mic track (and its processor) is gone — recompute to `no-mic`.
      this.emitAiNoiseFilter();
      this.settleVerifiedMicProcessing();
    }
    const track = publication.track;
    const kind = mapKind(publication.kind);
    if (!track || !kind) return;
    this.emit("localTrackUnpublished", {
      participantIdentity: participant.identity,
      publicationSid: publication.trackSid,
      kind,
      source: mapTrackSource(publication.source, kind),
      track,
    });
  };

  private handleParticipantConnected = (participant: RemoteParticipant) => {
    this.emit("participantConnected", participant.identity);
  };

  private handleParticipantDisconnected = (participant: RemoteParticipant) => {
    this.forgetParticipantSubscribedAudioTracks(participant.identity);
    this.emit("participantDisconnected", participant.identity);
  };

  private handleDisconnected = (reason?: unknown) => {
    this.clearParticipantAudioVolumes();
    // LiveKit destroys the processor when the track stops; just reset our
    // per-track failure guard + capture-NS state so the next call starts clean.
    this.aiNoiseFailedModels.clear();
    this.appliedModelActiveForCapture = false;
    this.emit("disconnected", typeof reason === "string" ? reason : undefined);
  };

  private handleAudioPlaybackStatusChanged = (canPlaybackAudio: boolean) => {
    this.emit("audioPlaybackStatusChanged", canPlaybackAudio);
  };

  /**
   * A mid-call device switch swaps the underlying capture track in
   * place via `switchActiveDevice` — it does NOT re-fire
   * `LocalTrackPublished`, so this is the only signal that the applied
   * audio processing may have changed (a new mic can support a
   * different set of constraints). Only `audioinput` matters; speaker /
   * camera changes can't alter mic processing.
   */
  private handleActiveDeviceChanged = (kind: MediaDeviceKind) => {
    if (kind !== "audioinput") return;
    this.emitMicAudioProcessing();
    // Defensive: LiveKit re-runs the processor across a device switch, but
    // reconcile is idempotent and re-attaches if the new track came up bare.
    void this.ensureAiNoiseFilter();
  };

  /**
   * Recompute when the local mic is muted or unmuted. Muting is the most
   * common mic transition, and with LiveKit's default
   * `stopMicTrackOnMute: false` it neither unpublishes nor ends the
   * capture track — so without this listener the indicator would keep
   * showing a stale trio for a mic that isn't capturing. Fires for
   * remote tracks too, hence the local-mic filter.
   */
  private handleTrackMuteChanged = (
    publication: TrackPublication,
    participant: Participant,
  ) => {
    if (!participant.isLocal) return;
    if (publication.source !== Track.Source.Microphone) return;
    this.emitMicAudioProcessing();
    // Unmute restarts the track; reconcile (idempotent) re-attaches if needed.
    void this.ensureAiNoiseFilter();
  };

  /**
   * Read the *applied* audio processing of the local mic from its live
   * capture track. `no-mic` when nothing is publishing, the mic is muted,
   * or the track has ended (e.g. the device was unplugged) — so the
   * indicator never reports a trio for a mic that isn't actually
   * capturing. A muted mic must be checked explicitly: LiveKit's default
   * `stopMicTrackOnMute: false` leaves the underlying track `live` while
   * muted, so the `readyState` guard alone would miss it.
   */
  private computeMicAudioProcessing(): MicAudioProcessing {
    const room = this.room;
    if (!room) return { kind: "no-mic" };
    const publication = room.localParticipant.getTrackPublication(Track.Source.Microphone);
    if (!publication || publication.isMuted) return { kind: "no-mic" };
    const mediaStreamTrack = publication.track?.mediaStreamTrack;
    if (!mediaStreamTrack || mediaStreamTrack.readyState !== "live") {
      return { kind: "no-mic" };
    }
    return activeMicAudioProcessing(mediaStreamTrack.getSettings());
  }

  private emitMicAudioProcessing(): void {
    this.emit("micAudioProcessingChanged", this.computeMicAudioProcessing());
  }

  /**
   * LiveKit re-scores connection quality for every participant; the
   * self-only indicator cares about the local one. Remote participants'
   * scores are dropped here rather than filtered downstream so the engine
   * surface stays self-scoped.
   */
  private handleConnectionQualityChanged = (
    quality: ConnectionQuality,
    participant: Participant,
  ) => {
    if (!participant.isLocal) return;
    this.emit("connectionQualityChanged", mapLiveKitConnectionQuality(quality));
  };

  private handleConnectionStateChanged = (state: ConnectionState) => {
    this.emit("connectionPhaseChanged", mapLiveKitConnectionState(state));
  };

  private clearParticipantAudioVolumes(): void {
    this.participantAudioVolumes = {};
    this.subscribedAudioTracks.clear();
  }

  private rememberSubscribedAudioTrack(
    identity: ParticipantAudioVolumeIdentity & { track: RemoteTrack },
  ): void {
    if (!isVolumeAdjustableTrack(identity.track)) return;
    const key = participantAudioVolumeKey(identity);
    const tracks = this.subscribedAudioTracks.get(key) ?? new Set<VolumeAdjustableTrack>();
    tracks.add(identity.track);
    this.subscribedAudioTracks.set(key, tracks);
    identity.track.setVolume(resolveParticipantAudioVolume(this.participantAudioVolumes, identity));
  }

  private forgetSubscribedAudioTrack(
    identity: ParticipantAudioVolumeIdentity & { track: RemoteTrack },
  ): void {
    if (!isVolumeAdjustableTrack(identity.track)) return;
    const key = participantAudioVolumeKey(identity);
    const tracks = this.subscribedAudioTracks.get(key);
    if (!tracks) return;
    tracks.delete(identity.track);
    if (tracks.size === 0) this.subscribedAudioTracks.delete(key);
  }

  private forgetParticipantSubscribedAudioTracks(participantIdentity: string): void {
    const sources: CallAudioTrackSource[] = ["microphone", "screen_share_audio"];
    for (const source of sources) {
      this.subscribedAudioTracks.delete(participantAudioVolumeKey({ participantIdentity, source }));
    }
  }
}

function isVolumeAdjustableTrack(track: RemoteTrack): track is RemoteTrack & VolumeAdjustableTrack {
  return "setVolume" in track && typeof track.setVolume === "function";
}

function isCallAudioTrackSource(source: CallTrackSource): source is CallAudioTrackSource {
  return source === "microphone" || source === "screen_share_audio";
}

function normalizeParticipantAudioVolume(volume: number | undefined): number {
  if (volume === undefined || !Number.isFinite(volume)) return 1;
  if (volume < 0) return 0;
  if (volume > 2) return 2;
  return volume;
}

function mapKind(kind: Track.Kind): "audio" | "video" | null {
  if (kind === Track.Kind.Audio) return "audio";
  if (kind === Track.Kind.Video) return "video";
  return null;
}

export function mapTrackSource(
  source: Track.Source,
  fallbackKind: "audio" | "video" = "video",
): CallTrackSource {
  if (source === Track.Source.Microphone) return "microphone";
  if (source === Track.Source.ScreenShare) return "screen_share";
  if (source === Track.Source.ScreenShareAudio) return "screen_share_audio";
  return fallbackKind === "audio" ? "microphone" : "camera";
}

/**
 * Translate LiveKit's `ConnectionQuality` enum into the SDK-free domain
 * union the UI consumes. The boundary lives here (the engine wraps the
 * `Room`), so `connection-quality.ts` stays free of `livekit-client`.
 */
export function mapLiveKitConnectionQuality(
  quality: ConnectionQuality,
): CallConnectionQuality {
  switch (quality) {
    case ConnectionQuality.Excellent:
      return "excellent";
    case ConnectionQuality.Good:
      return "good";
    case ConnectionQuality.Poor:
      return "poor";
    case ConnectionQuality.Lost:
      return "lost";
    default:
      return "unknown";
  }
}

/**
 * Distil LiveKit's `ConnectionState` into the transport phase the
 * indicator cares about. Both ICE (`Reconnecting`) and signal
 * (`SignalReconnecting`) recovery collapse to `reconnecting`; everything
 * pre-`Connected` is `disconnected`, which pairs with the `unknown`
 * quality to render the chip's quiet "measuring" bars.
 */
export function mapLiveKitConnectionState(
  state: ConnectionState,
): CallConnectionPhase {
  switch (state) {
    case ConnectionState.Connected:
      return "connected";
    case ConnectionState.Reconnecting:
    case ConnectionState.SignalReconnecting:
      return "reconnecting";
    default:
      return "disconnected";
  }
}

function isUnsupportedScreenAudioError(error: unknown): boolean {
  if (!error || typeof error !== "object" || !("name" in error)) return false;
  const name = (error as { name?: unknown }).name;
  return name === "OverconstrainedError" ||
    name === "ConstraintNotSatisfiedError" ||
    name === "NotSupportedError";
}

/**
 * The capability-providing subset of LiveKit's `LocalParticipant` that
 * [`enableRequestedCapture`] touches. Narrowed to a structural type so
 * the best-effort capture logic is unit-testable without a real
 * `Room` (which reaches for browser-only globals at construction).
 */
type CapturableParticipant = {
  setMicrophoneEnabled(enabled: boolean): Promise<unknown>;
  setCameraEnabled(
    enabled: boolean,
    options?: VideoCaptureOptions,
    publishOptions?: TrackPublishOptions,
  ): Promise<unknown>;
};

/**
 * Enable the locally-requested tracks as a BEST-EFFORT step after the
 * room has already connected. A missing device or a denied permission
 * MUST NOT throw: the participant has already joined and is a fully
 * functional receive-only member, so only publishing failed. Mic and
 * camera are attempted independently — a broken camera never suppresses
 * a working mic, and neither failure aborts the join. Each failure is
 * reported through `onError` (the engine forwards it as
 * `mediaDevicesError`) so the UI can surface a non-blocking notice.
 */
export async function enableRequestedCapture(
  participant: CapturableParticipant,
  opts: { audio: boolean; video: boolean },
  onError: (source: "audio" | "video", error: unknown) => void,
  cameraPublishOptions: TrackPublishOptions,
): Promise<void> {
  if (opts.audio) {
    try {
      await participant.setMicrophoneEnabled(true);
    } catch (error) {
      onError("audio", error);
    }
  }
  if (opts.video) {
    try {
      await participant.setCameraEnabled(true, undefined, cameraPublishOptions);
    } catch (error) {
      onError("video", error);
    }
  }
}
