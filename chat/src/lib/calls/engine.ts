import {
  Room,
  RoomEvent,
  Track,
  type LocalParticipant,
  type LocalTrack,
  type LocalTrackPublication,
  type RemoteParticipant,
  type RemoteTrack,
  type RemoteTrackPublication,
} from "livekit-client";
import type { LiveKitJoin } from "./types";
import { $devicePrefs } from "./device-prefs";

/**
 * Identifies a remote media track surfaced to the UI. The `kind`
 * mirrors LiveKit's `Track.Kind` so the renderer can pick a
 * `<video>` vs `<audio>` element without re-deriving it.
 */
export type RemoteMediaTrack = {
  participantIdentity: string;
  publicationSid: string;
  kind: "audio" | "video";
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
  track: LocalTrack;
};

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
  participantDisconnected: (identity: string) => void;
  disconnected: (reason?: string) => void;
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
  private listeners: { [K in keyof CallEngineEvents]: Set<CallEngineEvents[K]> } = {
    trackSubscribed: new Set(),
    trackUnsubscribed: new Set(),
    localTrackPublished: new Set(),
    localTrackUnpublished: new Set(),
    participantDisconnected: new Set(),
    disconnected: new Set(),
  };

  /** LiveKit identity of the local participant, populated once
   *  `connect()` resolves. Exposed so the overlay can render the
   *  self-preview tile with the same label as remote tiles use. */
  get localIdentity(): string | null {
    return this.room?.localParticipant.identity ?? null;
  }

  async connect(join: LiveKitJoin, opts: { audio: boolean; video: boolean }): Promise<void> {
    if (this.room) throw new Error("CallEngine already connected");
    const prefs = $devicePrefs.get();
    const room = new Room({
      adaptiveStream: true,
      dynacast: true,
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
    room.on(RoomEvent.ParticipantDisconnected, this.handleParticipantDisconnected);
    room.on(RoomEvent.Disconnected, this.handleDisconnected);
    await room.connect(join.url, join.token);
    this.room = room;
    // Post-connect setup must be atomic from the caller's POV: if a
    // permission prompt is denied for the mic or cam, throwing without
    // first tearing down the half-initialized room would leave
    // `this.room` set against a connected LiveKit session that nothing
    // will ever close. Subsequent `connect()` calls would then fail on
    // the "already connected" guard for a session the user never sees.
    try {
      if (opts.audio) await room.localParticipant.setMicrophoneEnabled(true);
      if (opts.video) await room.localParticipant.setCameraEnabled(true);
      // Re-apply the speaker preference now that the room has remote
      // audio elements to retarget; the engine ignores it on connect
      // because no `<audio>` exists yet, but every subsequent
      // RoomEvent.TrackSubscribed wires through this.applySpeaker.
      if (prefs.speaker) {
        await this.applySpeakerDevice(prefs.speaker).catch(() => undefined);
      }
    } catch (err) {
      this.room = null;
      await room.disconnect().catch(() => undefined);
      throw err;
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
   * Uses LiveKit's built-in `audiooutput` switch for browsers that
   * implement `HTMLMediaElement.setSinkId`; falls back to a no-op on
   * browsers that don't (Firefox today).
   */
  async setSpeakerDevice(deviceId: string): Promise<void> {
    await this.applySpeakerDevice(deviceId);
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
    room.off(RoomEvent.ParticipantDisconnected, this.handleParticipantDisconnected);
    room.off(RoomEvent.Disconnected, this.handleDisconnected);
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
    this.emit("trackSubscribed", {
      participantIdentity: participant.identity,
      publicationSid: publication.trackSid,
      kind,
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
    this.emit("trackUnsubscribed", {
      participantIdentity: participant.identity,
      publicationSid: publication.trackSid,
      kind,
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
    this.emit("localTrackPublished", {
      participantIdentity: participant.identity,
      publicationSid: publication.trackSid,
      kind,
      track,
    });
  };

  private handleLocalTrackUnpublished = (
    publication: LocalTrackPublication,
    participant: LocalParticipant,
  ) => {
    const track = publication.track;
    const kind = mapKind(publication.kind);
    if (!track || !kind) return;
    this.emit("localTrackUnpublished", {
      participantIdentity: participant.identity,
      publicationSid: publication.trackSid,
      kind,
      track,
    });
  };

  private handleParticipantDisconnected = (participant: RemoteParticipant) => {
    this.emit("participantDisconnected", participant.identity);
  };

  private handleDisconnected = (reason?: unknown) => {
    this.emit("disconnected", typeof reason === "string" ? reason : undefined);
  };
}

function mapKind(kind: Track.Kind): "audio" | "video" | null {
  if (kind === Track.Kind.Audio) return "audio";
  if (kind === Track.Kind.Video) return "video";
  return null;
}
