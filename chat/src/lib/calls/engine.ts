import {
  ConnectionQuality,
  ConnectionState,
  DisconnectReason,
  Room,
  RoomEvent,
  Track,
  type LocalParticipant,
  type LocalTrack,
  type LocalTrackPublication,
  type Participant,
  type RemoteParticipant,
  type RemoteTrack,
  type RemoteTrackPublication,
  type TrackPublication,
  type AudioCaptureOptions,
  type RoomConnectOptions,
  type RoomOptions,
  type RoomEventCallbacks,
  type TrackPublishOptions,
  type VideoCaptureOptions,
} from "livekit-client";
import { videoPublishPlan, type CameraPublishPlan } from "./video-codec/video-publish";
import { audioPublishOptions } from "./audio-publish";
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
  type ResolvedCallDevicePreference,
  missingCallDeviceError,
  resolveCallDevicePreference,
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
import {
  BACKGROUND_OFF,
  CAMERA_BACKGROUND_PROCESSOR_NAME,
  type ActiveBackgroundEffect,
  type BackgroundEffect,
} from "./background-effect/effect-id";
import {
  runBackgroundEffectReconcile,
  type BackgroundProcessorTarget,
} from "./background-effect/reconcile";
import type { CameraBackgroundState } from "./background-effect/camera-background";
import type { CameraBackgroundOps, VideoBackgroundProcessor } from "./background-effect/ops";
import { cameraBackgroundOps } from "./background-effect/registry";
import type { IceServerBundle } from "./ice-servers";
import {
  createIceCredentialRefresher,
  type IceCredentialRefresher,
} from "./ice-credential-refresh";
import type { TimerClock } from "./timer-clock";

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
 * The local camera track surface the background reconciler drives — the subset
 * of `LocalVideoTrack` it needs. Structural so the engine can build a
 * `BackgroundProcessorTarget` from a real track and tests can inject a fake.
 */
type CameraProcessorTrack = {
  getProcessor(): { name: string } | undefined;
  setProcessor(processor: VideoBackgroundProcessor): Promise<void>;
  stopProcessor(): Promise<void>;
};

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
    // Only the device selection lives here; the camera's 720p capture cap is set
    // per-publish by `videoPublishPlan` (so the builder owns the cap and LiveKit
    // merges it over this without overwriting `deviceId`). Screen-share
    // encoding/codec is likewise per-publish (capability-dependent), and the
    // mic's Opus voice-clarity profile is set per-publish by `audioPublishOptions`
    // (scoped to the mic so it never bleeds onto screen-share *system* audio,
    // which may be stereo). Hence no global `publishDefaults` here.
    videoCaptureDefaults: {
      deviceId: prefs.cam ?? undefined,
    },
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

function defaultCallDevicePreference(): ResolvedCallDevicePreference {
  return {
    activeDeviceId: "default",
    preferenceId: null,
    captureDeviceId: undefined,
    missing: false,
  };
}

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
  connected: (snapshot: {
    localIdentity: string;
    remoteIdentities: string[];
    /**
     * The LiveKit room name this call joined. Carried so telemetry can
     * derive the #1452 call correlation id from the one identifier the
     * client, the server call-setup path, and the inbound LiveKit
     * webhook all already share — no new wire field.
     */
    roomName: string;
  }) => void;
  disconnected: (info: {
    origin: "local" | "transport";
    reason?: DisconnectReason;
  }) => void;
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
  mediaDevicesError: (info: { source: "audio" | "video" | "screen"; error: unknown }) => void;
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
   * Fires when the *verified* camera background effect changes — on camera
   * publish/unpublish, a video device switch, or an explicit effect selection.
   * Carries the effect read back from the attached processor's presence
   * (`getProcessor()`), NOT merely the requested pref, so the indicator is
   * honest even when an attach silently fails. See `camera-background.ts`.
   */
  backgroundEffectChanged: (state: CameraBackgroundState) => void;
  /**
   * Fires when an effect fails to attach (asset/model fetch error, unsupported
   * device, image decode). NON-FATAL: the engine fails open to the raw camera
   * and the UI shows a non-blocking notice; the per-(track,effect) guard stops
   * the defensive re-runs from retrying until a fresh track or re-selection.
   */
  backgroundEffectError: (info: { effect: ActiveBackgroundEffect; error: unknown }) => void;
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
  /**
   * Fires on a full media-transport reconnect (`ConnectionState.Reconnecting`),
   * i.e. an actual ICE restart — deliberately NOT for
   * `SignalReconnecting`, which is only the signaling WebSocket
   * recovering while media keeps flowing (#1452 ICE-restart SLI).
   */
  transportReconnecting: () => void;
  /** XEP-0215 credentials were refreshed for a future full reconnect. */
  iceCredentialsRefreshed: () => void;
  /** The current XEP-0215 TURN credentials reached their expiry mid-call. */
  iceCredentialsExpired: () => void;
  /**
   * Fires when LiveKit re-derives which participants are actively speaking
   * (from audio levels). Carries the full speaking set — including the local
   * participant — as identities, so the UI can highlight the active speaker's
   * tile. LiveKit already debounces; the UI adds a brief hold on top to avoid
   * flicker. See `active-speakers.ts`.
   */
  activeSpeakersChanged: (identities: string[]) => void;
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
  /**
   * True from the start of `connect()` until `this.room` is assigned. The
   * `this.room` guard alone is set only AFTER `await room.connect`, so two
   * overlapping `connect()` calls (e.g. a hang-up-and-redial during the
   * pre-connect ICE fetch) would both pass it and each build a `Room`, leaking
   * one. This bridges that await window.
   */
  private connecting = false;
  /**
   * Bumped by every `connect()` and by `disconnect()`. A `connect()` whose
   * generation no longer matches once `room.connect` resolves was cancelled
   * mid-flight (a `disconnect()` or newer `connect()` superseded it); it tears
   * its own `Room` down instead of publishing an orphan. Without this, a
   * `disconnect()` racing the connect await sees `this.room === null`, returns,
   * and the in-flight `Room` later connects with nothing left to tear it down.
   */
  private connectGeneration = 0;
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
    backgroundEffectChanged: new Set(),
    backgroundEffectError: new Set(),
    verifiedMicProcessingChanged: new Set(),
    connectionQualityChanged: new Set(),
    connectionPhaseChanged: new Set(),
    transportReconnecting: new Set(),
    iceCredentialsRefreshed: new Set(),
    iceCredentialsExpired: new Set(),
    activeSpeakersChanged: new Set(),
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
   * The user's selected camera background effect, seeded from prefs at connect.
   * Source of "what should be attached" for the background reconciler.
   */
  private desiredBackgroundEffect: BackgroundEffect = BACKGROUND_OFF;
  /**
   * The effect last successfully applied to the live camera track. Combined
   * with the attached processor's presence to verify what is actually on —
   * reset to off when the camera track (and its processor) goes away.
   */
  private appliedBackgroundEffect: BackgroundEffect = BACKGROUND_OFF;
  /**
   * Effect keys that have failed to attach to the CURRENT camera track. Skips a
   * retry on every defensive re-run; cleared on a fresh publish or re-selection.
   */
  private backgroundFailedEffects = new Set<string>();
  /** Serializes reconciles so bursty triggers can't race on setProcessor. */
  private backgroundReconcileChain: Promise<void> = Promise.resolve();
  /** The library + asset boundary; injectable so tests avoid MediaPipe. */
  private readonly backgroundOps: CameraBackgroundOps;
  /**
   * Which video codecs this device can publish, probed once at construction.
   * Injectable so tests assert the forwarded options for capable vs.
   * incapable devices without real WebRTC.
   */
  private readonly codecSupport: VideoCodecSupport;
  /**
   * Builds the LiveKit `Room`; injectable so tests can assert the connect
   * options (notably the XEP-0215 `rtcConfig.iceServers`) without a real
   * `RTCPeerConnection`, which `new Room()` reaches for at construction.
   */
  private readonly makeRoom: (options: RoomOptions) => Room;
  private readonly iceRefreshClock: TimerClock | undefined;
  private iceCredentialRefresher: IceCredentialRefresher | null = null;
  private readonly scopedRoomListeners = new WeakMap<Room, {
    activeDeviceChanged: RoomEventCallbacks[RoomEvent.ActiveDeviceChanged];
    connectionStateChanged: RoomEventCallbacks[RoomEvent.ConnectionStateChanged];
    mediaDevicesChanged: RoomEventCallbacks[RoomEvent.MediaDevicesChanged];
    mediaDevicesError: RoomEventCallbacks[RoomEvent.MediaDevicesError];
  }>();

  constructor(opts?: {
    makeAiNoiseProcessor?: (model: NoiseModelId) => Promise<AudioNoiseProcessor>;
    backgroundOps?: CameraBackgroundOps;
    videoCodecSupport?: VideoCodecSupport;
    makeRoom?: (options: RoomOptions) => Room;
    iceRefreshClock?: TimerClock;
  }) {
    this.makeAiNoiseProcessor = opts?.makeAiNoiseProcessor ?? makeNoiseProcessor;
    this.backgroundOps = opts?.backgroundOps ?? cameraBackgroundOps;
    this.codecSupport = opts?.videoCodecSupport ?? videoCodecSupport(currentVideoCodecSupportEnv());
    this.makeRoom = opts?.makeRoom ?? ((options) => new Room(options));
    this.iceRefreshClock = opts?.iceRefreshClock;
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

  activeDeviceId(kind: MediaDeviceKind): string | null {
    return this.room?.getActiveDevice(kind) ?? null;
  }

  async connect(
    join: LiveKitJoin,
    opts: {
      audio: boolean;
      video: boolean;
      iceServers?: RTCIceServer[];
      iceServersExpiryMs?: number | null;
      refreshIceServers?: () => Promise<IceServerBundle>;
    },
  ): Promise<void> {
    if (this.room || this.connecting) throw new Error("CallEngine already connected");
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
    this.aiNoiseFailedModels = new Set();
    this.appliedModelActiveForCapture = false;
    // Seed the desired background effect from prefs so it re-applies on this
    // call's camera publish; start each call with a clean failure guard.
    this.desiredBackgroundEffect = prefs.backgroundEffect;
    this.backgroundFailedEffects = new Set();
    this.appliedBackgroundEffect = BACKGROUND_OFF;
    this.connecting = true;
    const generation = ++this.connectGeneration;
    try {
      // Preserve the synchronous all-default path: only an actual saved id
      // needs enumeration. This keeps disconnect-during-connect semantics from
      // acquiring an extra microtask window before the Room is constructed.
      let resolvedMic = defaultCallDevicePreference();
      let resolvedCam = defaultCallDevicePreference();
      let resolvedSpeaker = defaultCallDevicePreference();
      if (prefs.mic !== null || prefs.cam !== null || prefs.speaker !== null) {
        [resolvedMic, resolvedCam, resolvedSpeaker] = await Promise.all([
          resolveCallDevicePreference("mic", prefs.mic),
          resolveCallDevicePreference("cam", prefs.cam),
          resolveCallDevicePreference("speaker", prefs.speaker),
        ]);
      }
      if (this.connectGeneration !== generation) return;
      const room = this.makeRoom(
        callRoomOptionsForPrefs({
          mic: resolvedMic.captureDeviceId ?? null,
          cam: resolvedCam.captureDeviceId ?? null,
          audioProcessing: prefs.audioProcessing,
        }),
      );
      this.updateRoomListeners(room, "on");
      // XEP-0215 ICE injection: when the server advertised external services,
      // make them the authoritative ICE list via `rtcConfig`. An empty/absent
      // list means "no advertisement" — pass no `rtcConfig` so LiveKit keeps its
      // own signalling-provided servers rather than connecting with none.
      // livekit-client 2.19.1 exposes rtcConfig only at connect time: Room has
      // no public setConfiguration/restartIce API. Keep this exact mutable
      // object alive because the SDK retains it as RTCEngine.rtcConfig and
      // clones it when a FULL reconnect rebuilds its PeerConnections. Updating
      // it below therefore supplies fresh credentials to a later rebuild, but
      // deliberately cannot alter the already-running PeerConnection.
      // No advertisement (or a failed fetch) = no rtcConfig at all — and no
      // refresher either: with nothing to hand LiveKit at connect time there
      // is no retained config object for a refresh to update, and the call
      // never had a client-controlled relay to lose.
      const rtcConfig: RTCConfiguration | undefined =
        opts.iceServers && opts.iceServers.length > 0
          ? { iceServers: opts.iceServers }
          : undefined;
      const connectOptions: RoomConnectOptions | undefined = rtcConfig ? { rtcConfig } : undefined;
      try {
        await room.connect(join.url, join.token, connectOptions);
      } catch (err) {
        // Connect itself failed; the listeners we bound above would
        // otherwise dangle on a `Room` that nothing references but the
        // closure inside the listener — strip them so a future GC has
        // a clean path. `this.room` was never set so the caller stays
        // on the well-defined "not connected" path.
        room.removeAllListeners();
        throw err;
      }
      // A disconnect() (or a newer connect()) during the await bumped the
      // generation: this connect was cancelled. Tear our own Room down rather
      // than publishing it as an orphan that nothing will ever disconnect.
      if (this.connectGeneration !== generation) {
        room.removeAllListeners();
        await room.disconnect().catch(() => undefined);
        return;
      }
      this.room = room;
      this.connecting = false;
      if (opts.refreshIceServers && rtcConfig) {
        const refresher = createIceCredentialRefresher({
          refresh: opts.refreshIceServers,
          apply: (bundle) => {
            rtcConfig.iceServers = bundle.servers;
          },
          isCurrent: () => this.isCurrentRoom(room, generation),
          onRefreshed: () => this.emit("iceCredentialsRefreshed"),
          onExpired: () => this.emit("iceCredentialsExpired"),
          clock: this.iceRefreshClock,
        });
        this.iceCredentialRefresher = refresher;
        refresher.start(opts.iceServersExpiryMs ?? null);
      }
      // Emit the initial participant snapshot so subscribers can seed
      // their projection without missing peers that connected before
      // our listeners attached.
      this.emit("connected", {
        localIdentity: room.localParticipant.identity,
        remoteIdentities: Array.from(room.remoteParticipants.values()).map((p) => p.identity),
        roomName: room.name || join.room,
      });
      if (resolvedMic.missing) {
        this.emit("mediaDevicesError", {
          source: "audio",
          error: missingCallDeviceError("mic"),
        });
      }
      if (resolvedCam.missing) {
        this.emit("mediaDevicesError", {
          source: "video",
          error: missingCallDeviceError("cam"),
        });
      }
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
        (source, error) => {
          if (this.isCurrentRoom(room, generation)) {
            this.emit("mediaDevicesError", { source, error });
          }
        },
        {
          audio: audioPublishOptions(),
          camera: videoPublishPlan({ source: "camera", capability: this.codecSupport }),
        },
        () => this.isCurrentRoom(room, generation),
      );
      if (!this.isCurrentRoom(room, generation)) return;
      // Re-apply the speaker preference now that the room has remote
      // audio elements to retarget; the engine ignores it on connect
      // because no `<audio>` exists yet, but every subsequent
      // RoomEvent.TrackSubscribed wires through this.applySpeaker.
      if (resolvedSpeaker.preferenceId !== null) {
        await this.applySpeakerDevice(resolvedSpeaker.activeDeviceId).catch(() => undefined);
        if (!this.isCurrentRoom(room, generation)) return;
      }
    } finally {
      if (this.connectGeneration === generation && this.connecting) {
        this.connecting = false;
      }
    }
  }

  async setMicEnabled(enabled: boolean): Promise<void> {
    const room = this.room;
    if (!room) return;
    const generation = this.connectGeneration;
    // Forward the Opus voice-clarity profile (~64k mono, RED + DTX) so a mic
    // (re)enabled mid-call publishes at the same default as the initial join,
    // not LiveKit's lower `AudioPresets.music`. Scoped to the mic on purpose —
    // screen-share system audio keeps the SDK defaults. Inert on disable (mute).
    try {
      await room.localParticipant.setMicrophoneEnabled(enabled, undefined, audioPublishOptions());
    } catch (error) {
      if (!this.isCurrentRoom(room, generation)) return;
      throw error;
    }
    if (!this.isCurrentRoom(room, generation)) return;
  }

  async setCameraEnabled(enabled: boolean): Promise<void> {
    const room = this.room;
    if (!room) return;
    const generation = this.connectGeneration;
    // Forward the VP9+L3T3 / 720p camera plan — including the per-publish capture
    // cap — so a camera (re)enabled mid-call publishes at the same default as the
    // initial join, not LiveKit's 1080p/VP8 default.
    const { capture, publish } = videoPublishPlan({ source: "camera", capability: this.codecSupport });
    try {
      await room.localParticipant.setCameraEnabled(enabled, capture, publish);
    } catch (error) {
      if (!this.isCurrentRoom(room, generation)) return;
      throw error;
    }
    if (!this.isCurrentRoom(room, generation)) return;
  }

  async setScreenShareEnabled(
    enabled: boolean,
    opts: { audio: boolean },
  ): Promise<void> {
    const room = this.room;
    if (!room) return;
    const generation = this.connectGeneration;
    const { capture, publish } = videoPublishPlan({
      source: "screen",
      capability: this.codecSupport,
      screenCapture: currentScreenCaptureEnv(),
    });
    const captureOptions = { ...capture, audio: opts.audio };
    try {
      await room.localParticipant.setScreenShareEnabled(enabled, captureOptions, publish);
      if (!this.isCurrentRoom(room, generation)) return;
    } catch (error) {
      if (!this.isCurrentRoom(room, generation)) return;
      if (!enabled || !opts.audio || !isUnsupportedScreenAudioError(error)) throw error;
      try {
        await room.localParticipant.setScreenShareEnabled(
          true,
          { ...captureOptions, audio: false },
          publish,
        );
      } catch (fallbackError) {
        if (!this.isCurrentRoom(room, generation)) return;
        throw fallbackError;
      }
      if (!this.isCurrentRoom(room, generation)) return;
    }
  }

  /**
   * Switch the active microphone device mid-call. LiveKit's
   * `switchActiveDevice('audioinput', id)` rewrites the existing
   * publication in place — no re-publish, no Jingle round-trip — so
   * the change is invisible to the peer.
   */
  async setMicDevice(deviceId: string): Promise<void> {
    const room = this.room;
    if (!room) return;
    const generation = this.connectGeneration;
    const resolved = await resolveCallDevicePreference(
      "mic",
      deviceId === "default" ? null : deviceId,
    );
    if (!this.isCurrentRoom(room, generation)) return;
    try {
      await room.switchActiveDevice("audioinput", resolved.activeDeviceId);
    } catch (error) {
      if (!this.isCurrentRoom(room, generation)) return;
      throw error;
    }
    if (!this.isCurrentRoom(room, generation)) return;
    if (resolved.missing) {
      this.emit("mediaDevicesError", {
        source: "audio",
        error: missingCallDeviceError("mic"),
      });
    }
  }

  /**
   * Restart the live microphone capture with updated browser-native
   * audio-processing constraints. LiveKit swaps the capture track inside
   * the existing publication, so the call stays joined and no Jingle
   * signaling is needed. The effective constraints account for an active AI
   * model superseding the browser `noiseSuppression`.
   */
  async setAudioProcessing(prefs: AudioProcessingPrefs): Promise<void> {
    const room = this.room;
    if (!room) return;
    const generation = this.connectGeneration;
    await this.restartMicCapture(prefs, room, generation);
    if (!this.isCurrentRoom(room, generation)) return;
    this.settleVerifiedMicProcessing();
  }

  private async restartMicCapture(
    prefs: AudioProcessingPrefs,
    room: Room,
    generation: number,
  ): Promise<void> {
    const publication = room.localParticipant.getTrackPublication(Track.Source.Microphone);
    if (!publication || publication.isMuted) return;
    const track = publication.track;
    if (!track) return;
    const restartTrack = track?.restartTrack;
    if (typeof restartTrack !== "function") return;
    const settings = track.mediaStreamTrack?.getSettings();
    try {
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
    } catch (error) {
      if (!this.isCurrentRoom(room, generation)) return;
      throw error;
    }
    if (!this.isCurrentRoom(room, generation)) return;
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
    this.aiNoiseFailedModels = new Set();
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
    const room = this.room;
    if (!room) return;
    const generation = this.connectGeneration;
    const modelActive = this.verifiedAiNoiseModel() !== null;
    if (modelActive === this.appliedModelActiveForCapture) return;
    this.appliedModelActiveForCapture = modelActive;
    await this.restartMicCapture($devicePrefs.get().audioProcessing, room, generation);
    if (!this.isCurrentRoom(room, generation)) return;
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
    const room = this.room;
    const generation = this.connectGeneration;
    this.aiReconcileChain = this.aiReconcileChain
      .then(() => {
        if (!room || !this.isCurrentRoom(room, generation)) return;
        return this.runEnsureAiNoiseFilter(room, generation);
      })
      .catch(() => undefined);
    return this.aiReconcileChain;
  }

  private async runEnsureAiNoiseFilter(room: Room, generation: number): Promise<void> {
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
      attach: async (processor) => {
        if (!this.isCurrentRoom(room, generation)) {
          await processor.destroy().catch(() => undefined);
          return;
        }
        await track.setProcessor(processor);
        if (!this.isCurrentRoom(room, generation)) {
          await track.stopProcessor().catch(() => undefined);
        }
      },
      clear: async () => {
        if (!this.isCurrentRoom(room, generation)) return;
        await track.stopProcessor();
      },
    };
    const outcome = await runAiNoiseFilterReconcile({
      target,
      desired: this.desiredAiNoiseModel,
      makeProcessor: this.makeAiNoiseProcessor,
      failedModels: this.aiNoiseFailedModels,
    });
    if (!this.isCurrentRoom(room, generation)) return;
    if (outcome.action === "failed") {
      this.emit("aiNoiseFilterError", { model: outcome.model, error: outcome.error });
    }
    // Flip browser NS to match what actually attached, then surface the
    // settled verified state for the UI and the fleet beacon.
    await this.syncEffectiveCaptureConstraints();
    if (!this.isCurrentRoom(room, generation)) return;
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
   * Select the camera background effect (off / blur / image). Updates the
   * desired effect, gives the new selection a fresh attempt (clears the
   * per-track failure guard), then reconciles the attached processor.
   * Persistence is the caller's job (the device-selection layer writes the pref).
   */
  async setBackgroundEffect(effect: BackgroundEffect): Promise<void> {
    this.desiredBackgroundEffect = effect;
    this.backgroundFailedEffects = new Set();
    await this.ensureBackgroundEffect();
  }

  /**
   * The local camera track as a processor target, or null when no camera is
   * live / it is muted (nothing to attach to).
   */
  private cameraProcessorTrack(): CameraProcessorTrack | null {
    const publication = this.room?.localParticipant.getTrackPublication(Track.Source.Camera);
    if (!publication || publication.isMuted) return null;
    const track = publication.track as unknown as CameraProcessorTrack | undefined;
    return track && typeof track.getProcessor === "function" ? track : null;
  }

  /**
   * Reconcile the attached background processor toward the desired effect.
   * Idempotent and serialized; a no-op when no camera track exists (it re-runs
   * on the next publish). Fails open on attach error and emits a typed
   * `backgroundEffectError`.
   */
  private ensureBackgroundEffect(): Promise<void> {
    const room = this.room;
    const generation = this.connectGeneration;
    this.backgroundReconcileChain = this.backgroundReconcileChain
      .then(() => {
        if (!room || !this.isCurrentRoom(room, generation)) return;
        return this.runEnsureBackgroundEffect(room, generation);
      })
      .catch(() => undefined);
    return this.backgroundReconcileChain;
  }

  private async runEnsureBackgroundEffect(room: Room, generation: number): Promise<void> {
    const track = this.cameraProcessorTrack();
    if (!track) {
      // No live camera to attach to — surface the honest `no-camera` state; the
      // desired effect re-applies on the next publish.
      this.emitBackgroundEffect();
      return;
    }
    const target: BackgroundProcessorTarget = {
      currentEffect: () =>
        track.getProcessor()?.name === CAMERA_BACKGROUND_PROCESSOR_NAME
          ? this.appliedBackgroundEffect
          : BACKGROUND_OFF,
      attach: async (effect) => {
        const processor = await this.backgroundOps.create(effect);
        if (!this.isCurrentRoom(room, generation)) {
          await processor.destroy().catch(() => undefined);
          return;
        }
        await track.setProcessor(processor);
        if (!this.isCurrentRoom(room, generation)) {
          await track.stopProcessor().catch(() => undefined);
          return;
        }
        this.appliedBackgroundEffect = effect;
      },
      switch: async (effect) => {
        if (!this.isCurrentRoom(room, generation)) return;
        const processor = track.getProcessor() as unknown as VideoBackgroundProcessor | undefined;
        // The reconciler only decides `switch` when a processor was verifiably
        // present (its name was just read), so this is effectively always set.
        // Guard defensively against SDK non-determinism: if the processor
        // vanished we must NOT claim the new effect applied — leave
        // `appliedBackgroundEffect` reflecting the processor's actual (unchanged)
        // effect, so the next reconcile retries the switch honestly.
        if (!processor) return;
        await this.backgroundOps.switch(processor, effect);
        if (!this.isCurrentRoom(room, generation)) return;
        this.appliedBackgroundEffect = effect;
      },
      clear: async () => {
        if (!this.isCurrentRoom(room, generation)) return;
        await track.stopProcessor();
        if (!this.isCurrentRoom(room, generation)) return;
        this.appliedBackgroundEffect = BACKGROUND_OFF;
      },
    };
    const outcome = await runBackgroundEffectReconcile({
      target,
      desired: this.desiredBackgroundEffect,
      failed: this.backgroundFailedEffects,
    });
    if (!this.isCurrentRoom(room, generation)) return;
    if (outcome.action === "failed") {
      this.emit("backgroundEffectError", { effect: outcome.effect, error: outcome.error });
    }
    this.emitBackgroundEffect();
  }

  /** Verified background effect read from the live processor's presence. */
  private computeCameraBackground(): CameraBackgroundState {
    const track = this.cameraProcessorTrack();
    if (!track) return { kind: "no-camera" };
    const present = track.getProcessor()?.name === CAMERA_BACKGROUND_PROCESSOR_NAME;
    return { kind: "active", effect: present ? this.appliedBackgroundEffect : BACKGROUND_OFF };
  }

  private emitBackgroundEffect(): void {
    this.emit("backgroundEffectChanged", this.computeCameraBackground());
  }

  /**
   * Same shape as `setMicDevice` but for the camera device.
   */
  async setCameraDevice(deviceId: string): Promise<void> {
    const room = this.room;
    if (!room) return;
    const generation = this.connectGeneration;
    const resolved = await resolveCallDevicePreference(
      "cam",
      deviceId === "default" ? null : deviceId,
    );
    if (!this.isCurrentRoom(room, generation)) return;
    try {
      await room.switchActiveDevice("videoinput", resolved.activeDeviceId);
    } catch (error) {
      if (!this.isCurrentRoom(room, generation)) return;
      throw error;
    }
    if (!this.isCurrentRoom(room, generation)) return;
    if (resolved.missing) {
      this.emit("mediaDevicesError", {
        source: "video",
        error: missingCallDeviceError("cam"),
      });
    }
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
    const room = this.room;
    if (!room) return;
    const generation = this.connectGeneration;
    try {
      await room.startAudio();
    } catch (error) {
      if (!this.isCurrentRoom(room, generation)) return;
      throw error;
    }
    if (!this.isCurrentRoom(room, generation)) return;
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
    const room = this.room;
    if (!room) return;
    const generation = this.connectGeneration;
    if (typeof room.switchActiveDevice !== "function") return;
    const resolved = await resolveCallDevicePreference(
      "speaker",
      deviceId === "default" ? null : deviceId,
    );
    if (!this.isCurrentRoom(room, generation)) return;
    try {
      await room.switchActiveDevice("audiooutput", resolved.activeDeviceId);
      if (!this.isCurrentRoom(room, generation)) return;
    } catch {
      // Browser doesn't support sinkId; the user already saw the
      // disabled chip in the picker. Swallow silently here.
    }
  }

  async disconnect(): Promise<void> {
    // Cancel any in-flight connect(): bump the generation so a connect still
    // awaiting `room.connect` tears its own Room down instead of publishing it,
    // and release the reservation so the engine can never wedge "connecting".
    this.connectGeneration++;
    this.connecting = false;
    this.stopIceCredentialRefresh();
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
    this.emit("disconnected", { origin: "local" });
    this.updateRoomListeners(room, "off");
    const processorsStopped = this.stopRoomProcessors(room);
    const roomDisconnected = room.disconnect();
    await processorsStopped;
    await roomDisconnected;
  }

  private isCurrentRoom(room: Room, generation: number): boolean {
    return this.connectGeneration === generation && this.room === room;
  }

  private stopIceCredentialRefresh(): void {
    this.iceCredentialRefresher?.stop();
    this.iceCredentialRefresher = null;
  }

  private updateRoomListeners(room: Room, operation: "on" | "off"): void {
    const update = <K extends keyof RoomEventCallbacks>(
      event: K,
      listener: RoomEventCallbacks[K],
    ): void => {
      if (operation === "on") room.on(event, listener);
      else room.off(event, listener);
    };
    let scoped = this.scopedRoomListeners.get(room);
    if (operation === "on") {
      const generation = this.connectGeneration;
      scoped = {
        activeDeviceChanged: (kind) => {
          if (this.isCurrentRoom(room, generation)) this.handleActiveDeviceChanged(kind);
        },
        connectionStateChanged: (state) => {
          if (this.isCurrentRoom(room, generation)) this.handleConnectionStateChanged(state);
        },
        mediaDevicesChanged: () => {
          if (this.isCurrentRoom(room, generation)) this.handleMediaDevicesChanged();
        },
        mediaDevicesError: (error, kind) => {
          if (this.isCurrentRoom(room, generation)) this.handleMediaDevicesError(error, kind);
        },
      };
      this.scopedRoomListeners.set(room, scoped);
    }
    update(RoomEvent.TrackSubscribed, this.handleTrackSubscribed);
    update(RoomEvent.TrackUnsubscribed, this.handleTrackUnsubscribed);
    update(RoomEvent.LocalTrackPublished, this.handleLocalTrackPublished);
    update(RoomEvent.LocalTrackUnpublished, this.handleLocalTrackUnpublished);
    update(RoomEvent.ParticipantConnected, this.handleParticipantConnected);
    update(RoomEvent.ParticipantDisconnected, this.handleParticipantDisconnected);
    update(RoomEvent.Disconnected, this.handleDisconnected);
    update(RoomEvent.AudioPlaybackStatusChanged, this.handleAudioPlaybackStatusChanged);
    if (scoped) {
      update(RoomEvent.ActiveDeviceChanged, scoped.activeDeviceChanged);
      update(RoomEvent.MediaDevicesChanged, scoped.mediaDevicesChanged);
      update(RoomEvent.MediaDevicesError, scoped.mediaDevicesError);
    }
    update(RoomEvent.TrackMuted, this.handleTrackMuteChanged);
    update(RoomEvent.TrackUnmuted, this.handleTrackMuteChanged);
    update(RoomEvent.ConnectionQualityChanged, this.handleConnectionQualityChanged);
    if (scoped) update(RoomEvent.ConnectionStateChanged, scoped.connectionStateChanged);
    update(RoomEvent.ActiveSpeakersChanged, this.handleActiveSpeakersChanged);
    if (operation === "off") this.scopedRoomListeners.delete(room);
  }

  private stopRoomProcessors(room: Room): Promise<void> {
    const stops: Array<Promise<unknown>> = [];
    const seen = new Set<ProcessorCapableTrack | CameraProcessorTrack>();
    const sources = [Track.Source.Microphone, Track.Source.Camera] as const;
    for (const source of sources) {
      const track = room.localParticipant?.getTrackPublication?.(source)
        ?.track as unknown as ProcessorCapableTrack | CameraProcessorTrack | undefined;
      if (
        track &&
        !seen.has(track) &&
        typeof track.getProcessor === "function" &&
        track.getProcessor()
      ) {
        seen.add(track);
        stops.push(track.stopProcessor().catch(() => undefined));
      }
    }
    return Promise.all(stops).then(() => undefined);
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
    if (source === "camera") {
      // A brand-new camera track: clear the per-track failure guard so an effect
      // that failed on a previous track gets one clean attempt, then re-apply the
      // still-desired effect to the fresh (bare) capture track.
      this.backgroundFailedEffects.clear();
      void this.ensureBackgroundEffect();
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
    if (publication.source === Track.Source.Camera) {
      // The camera track (and its processor) is gone — drop the applied effect
      // so a re-publish re-attaches fresh, and recompute to `no-camera`.
      this.appliedBackgroundEffect = BACKGROUND_OFF;
      this.emitBackgroundEffect();
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

  private handleDisconnected = async (reason?: DisconnectReason) => {
    const room = this.room;
    if (!room) return;
    this.connectGeneration++;
    this.connecting = false;
    this.stopIceCredentialRefresh();
    this.room = null;
    this.updateRoomListeners(room, "off");
    this.clearParticipantAudioVolumes();
    // LiveKit destroys the processor when the track stops; just reset our
    // per-track failure guard + capture-NS state so the next call starts clean.
    this.aiNoiseFailedModels = new Set();
    this.appliedModelActiveForCapture = false;
    // LiveKit destroys the camera processor with the track on disconnect; reset
    // our per-track guard + applied effect so the next call starts clean.
    this.backgroundFailedEffects = new Set();
    this.appliedBackgroundEffect = BACKGROUND_OFF;
    const processorsStopped = this.stopRoomProcessors(room);
    this.emit("disconnected", { origin: "transport", reason });
    await processorsStopped;
  };

  private handleAudioPlaybackStatusChanged = (canPlaybackAudio: boolean) => {
    this.emit("audioPlaybackStatusChanged", canPlaybackAudio);
  };

  private reconcileActiveDevice(kind: MediaDeviceKind): void {
    if (kind === "videoinput") {
      // A camera swap rewrites the publication's capture track in place. LiveKit
      // re-runs the processor across it, but reconcile is idempotent and
      // re-attaches the desired effect if the new track came up bare.
      void this.ensureBackgroundEffect();
      return;
    }
    if (kind !== "audioinput") return;
    this.emitMicAudioProcessing();
    // Defensive: LiveKit re-runs the processor across a device switch, but
    // reconcile is idempotent and re-attaches if the new track came up bare.
    void this.ensureAiNoiseFilter();
  }

  /**
   * A mid-call device switch swaps the underlying capture track in
   * place via `switchActiveDevice` — it does NOT re-fire
   * `LocalTrackPublished`, so this is the only signal that the applied
   * audio processing may have changed (a new mic can support a
   * different set of constraints). Only `audioinput` matters; speaker /
   * camera changes can't alter mic processing.
   */
  private handleActiveDeviceChanged = (kind: MediaDeviceKind) => {
    this.reconcileActiveDevice(kind);
  };

  private handleMediaDevicesChanged(): void {
    this.reconcileActiveDevice("audioinput");
    this.reconcileActiveDevice("videoinput");
  }

  private handleMediaDevicesError(
    error: Error,
    kind?: MediaDeviceKind,
  ): void {
    if (kind === "audioinput") {
      this.emit("mediaDevicesError", { source: "audio", error });
      return;
    }
    if (kind === "videoinput") {
      this.emit("mediaDevicesError", { source: "video", error });
      return;
    }
    // Playback-sink failures already surface via audioPlaybackStatusChanged.
    if (kind === "audiooutput") return;
    // livekit-client's sourceToKind maps ScreenShare (and Unknown) to an
    // undefined kind. Screen capture is the only remaining local source, so
    // an SDK-initiated screen failure (e.g. a track restart) surfaces there
    // instead of being dropped without any actionable UI.
    this.emit("mediaDevicesError", { source: "screen", error });
  }

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
    if (publication.source === Track.Source.Camera) {
      // Camera mute stops the capture track, so an attached effect is no longer
      // live — surface the honest `no-camera` state (mirrors the mic's no-mic),
      // and re-reconcile on unmute, where the processor may come back bare.
      this.emitBackgroundEffect();
      void this.ensureBackgroundEffect();
      return;
    }
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

  private handleConnectionStateChanged = (state: ConnectionState): void => {
    if (state === ConnectionState.Reconnecting) {
      // No public live-update API exists in livekit-client 2.19. Refresh now
      // so the retained rtcConfig is ready if this reconnect becomes a full
      // PeerConnection rebuild; the active PC itself is not reconfigured.
      void this.iceCredentialRefresher?.refreshNow();
      this.emit("transportReconnecting");
    }
    this.emit("connectionPhaseChanged", mapLiveKitConnectionState(state));
  };

  /**
   * LiveKit re-derived the active-speaker set from audio levels. Surface the
   * speaking identities (local participant included) so the UI can highlight
   * the talking tile; the brief anti-flicker hold lives in the view layer.
   */
  private handleActiveSpeakersChanged = (speakers: Participant[]) => {
    this.emit("activeSpeakersChanged", speakers.map((participant) => participant.identity));
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
  setMicrophoneEnabled(
    enabled: boolean,
    options?: AudioCaptureOptions,
    publishOptions?: TrackPublishOptions,
  ): Promise<unknown>;
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
  publish: { audio: TrackPublishOptions; camera: CameraPublishPlan },
  isCurrent: () => boolean = () => true,
): Promise<void> {
  if (opts.audio) {
    try {
      await participant.setMicrophoneEnabled(true, undefined, publish.audio);
    } catch (error) {
      onError("audio", error);
    }
    if (!isCurrent()) return;
  }
  if (opts.video) {
    try {
      // The camera carries both its capture cap and codec/SVC publish options;
      // forward both so the initial join publishes the 720p VP9 plan.
      await participant.setCameraEnabled(true, publish.camera.capture, publish.camera.publish);
    } catch (error) {
      onError("video", error);
    }
    if (!isCurrent()) return;
  }
}
