import type { LocalMediaTrack } from "./engine";
import type { TileAttachable } from "./tile-attach";

export type LocalScreenSharePresentation = {
  message: "You're sharing your screen";
  thumbnail: {
    attachKey: string;
    label: "Your screen";
    mirrorVideo: false;
    videoTrack: TileAttachable;
  };
  stageLocalTracks: readonly LocalMediaTrack[];
};

export function localScreenSharePresentation(input: {
  screenShareEnabled: boolean;
  localTracks: readonly LocalMediaTrack[];
}): LocalScreenSharePresentation | null {
  if (!input.screenShareEnabled) return null;
  const screenTrack = input.localTracks.find(
    (track) => track.kind === "video" && track.source === "screen_share",
  );
  if (!screenTrack) return null;
  return {
    message: "You're sharing your screen",
    thumbnail: {
      attachKey: `self:${screenTrack.participantIdentity}:screen_share`,
      label: "Your screen",
      mirrorVideo: false,
      videoTrack: screenTrack.track,
    },
    stageLocalTracks: input.localTracks.filter((track) => !isScreenShareSource(track)),
  };
}

function isScreenShareSource(track: LocalMediaTrack): boolean {
  return track.source === "screen_share" || track.source === "screen_share_audio";
}
