import type { RemoteMediaTrack } from "./engine";
import { TileAttachments } from "./tile-attach";

export type CallAudioSinkTrack = RemoteMediaTrack & {
  kind: "audio";
  source: "microphone" | "screen_share_audio";
};

export function callAudioSinkTracks(
  remoteTracks: readonly RemoteMediaTrack[],
): CallAudioSinkTrack[] {
  return remoteTracks.filter(isCallAudioSinkTrack);
}

export function callAudioSinkTrackKey(track: CallAudioSinkTrack): string {
  return `call-audio:${track.participantIdentity}:${track.publicationSid}`;
}

export class CallAudioSinkAttachments {
  private readonly attachments = new TileAttachments();

  sync(track: CallAudioSinkTrack, el: HTMLMediaElement | null): void {
    this.attachments.sync(callAudioSinkTrackKey(track), el, track.track);
  }

  detachAll(): void {
    this.attachments.detachAll();
  }

  size(): number {
    return this.attachments.size();
  }
}

function isCallAudioSinkTrack(track: RemoteMediaTrack): track is CallAudioSinkTrack {
  return track.kind === "audio" &&
    (track.source === "microphone" || track.source === "screen_share_audio");
}
