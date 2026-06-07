import {
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
} from "livekit-client";
import type { LiveKitJoin } from "./types";
import { $devicePrefs } from "./device-prefs";
import { validateLiveKitGrant } from "./dm-call-activity";
import { activeMicAudioProcessing, type MicAudioProcessing } from "./mic-audio-processing";

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
  };

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
    const room = new Room({
      adaptiveStream: true,
      dynacast: true,
      webAudioMix: true,
      audioCaptureDefaults: {
        autoGainControl: true,
        echoCancellation: true,
        noiseSuppression: true,
        deviceId: prefs.mic ?? undefined,
      },
      videoCaptureDefaults: {
        deviceId: prefs.cam ?? undefined,
      },
    });
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
    await this.room.localParticipant.setCameraEnabled(enabled);
  }

  async setScreenShareEnabled(
    enabled: boolean,
    opts: { audio: boolean },
  ): Promise<void> {
    if (!this.room) return;
    try {
      await this.room.localParticipant.setScreenShareEnabled(enabled, {
        audio: opts.audio,
      });
    } catch (error) {
      if (!enabled || !opts.audio || !isUnsupportedScreenAudioError(error)) throw error;
      await this.room.localParticipant.setScreenShareEnabled(true, { audio: false });
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
    if (source === "microphone") this.emitMicAudioProcessing();
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
    if (publication.source === Track.Source.Microphone) this.emitMicAudioProcessing();
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
  setCameraEnabled(enabled: boolean): Promise<unknown>;
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
      await participant.setCameraEnabled(true);
    } catch (error) {
      onError("video", error);
    }
  }
}
