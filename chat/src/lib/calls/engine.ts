import {
  Room,
  RoomEvent,
  Track,
  type RemoteParticipant,
  type RemoteTrack,
  type RemoteTrackPublication,
} from "livekit-client";
import type { LiveKitJoin } from "./types";

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

export type CallEngineEvents = {
  trackSubscribed: (track: RemoteMediaTrack) => void;
  trackUnsubscribed: (track: RemoteMediaTrack) => void;
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
    participantDisconnected: new Set(),
    disconnected: new Set(),
  };

  async connect(join: LiveKitJoin, opts: { audio: boolean; video: boolean }): Promise<void> {
    if (this.room) throw new Error("CallEngine already connected");
    const room = new Room({
      adaptiveStream: true,
      dynacast: true,
      audioCaptureDefaults: { autoGainControl: true, echoCancellation: true, noiseSuppression: true },
    });
    room.on(RoomEvent.TrackSubscribed, this.handleTrackSubscribed);
    room.on(RoomEvent.TrackUnsubscribed, this.handleTrackUnsubscribed);
    room.on(RoomEvent.ParticipantDisconnected, this.handleParticipantDisconnected);
    room.on(RoomEvent.Disconnected, this.handleDisconnected);
    await room.connect(join.url, join.token);
    this.room = room;
    if (opts.audio) await room.localParticipant.setMicrophoneEnabled(true);
    if (opts.video) await room.localParticipant.setCameraEnabled(true);
  }

  async setMicEnabled(enabled: boolean): Promise<void> {
    if (!this.room) return;
    await this.room.localParticipant.setMicrophoneEnabled(enabled);
  }

  async setCameraEnabled(enabled: boolean): Promise<void> {
    if (!this.room) return;
    await this.room.localParticipant.setCameraEnabled(enabled);
  }

  /** Attach the local camera preview to an HTMLVideoElement, if a camera track exists. */
  attachLocalCamera(target: HTMLVideoElement): void {
    if (!this.room) return;
    const pub = this.room.localParticipant.getTrackPublication(Track.Source.Camera);
    pub?.videoTrack?.attach(target);
  }

  async disconnect(): Promise<void> {
    const room = this.room;
    this.room = null;
    if (!room) return;
    room.off(RoomEvent.TrackSubscribed, this.handleTrackSubscribed);
    room.off(RoomEvent.TrackUnsubscribed, this.handleTrackUnsubscribed);
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
