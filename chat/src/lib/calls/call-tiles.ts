import type { LocalMediaTrack, RemoteMediaTrack } from "./engine";
import type { TileAttachable } from "./tile-attach";

export type CallTileModel = {
  key: string;
  identity: string;
  label: string;
  isSelf: boolean;
  mirrorVideo: boolean;
  micEnabledHint: boolean;
  videoTrack: TileAttachable | null;
  audioTrack: TileAttachable | null;
};

type TileSource = "camera" | "screen_share";

function displayNameForIdentity(identity: string): string {
  const at = identity.indexOf("@");
  if (at <= 0) return identity;
  return identity.slice(0, at);
}

function callTileKey(
  side: "self" | "remote",
  identity: string,
  source: TileSource,
): string {
  return `${side}:${identity}:${source}`;
}

function tileSourceForTrack(
  source: LocalMediaTrack["source"] | RemoteMediaTrack["source"],
): TileSource {
  return source === "screen_share" || source === "screen_share_audio"
    ? "screen_share"
    : "camera";
}

export function buildCallTiles(options: {
  remoteTracks: RemoteMediaTrack[];
  localTracks: LocalMediaTrack[];
  localIdentity: string | null;
  micEnabled: boolean;
}): CallTileModel[] {
  const byKey = new Map<string, CallTileModel>();
  const selfId =
    options.localTracks[0]?.participantIdentity ?? options.localIdentity ?? "you";
  byKey.set(callTileKey("self", selfId, "camera"), {
    key: callTileKey("self", selfId, "camera"),
    identity: selfId,
    label: "You",
    isSelf: true,
    mirrorVideo: true,
    micEnabledHint: options.micEnabled,
    videoTrack: null,
    audioTrack: null,
  });

  for (const local of options.localTracks) {
    const key = callTileKey("self", local.participantIdentity, tileSourceForTrack(local.source));
    const existing = byKey.get(key) ?? {
      key,
      identity: local.participantIdentity,
      label: "You",
      isSelf: true,
      mirrorVideo: tileSourceForTrack(local.source) === "camera",
      micEnabledHint: options.micEnabled,
      videoTrack: null,
      audioTrack: null,
    };
    if (local.kind === "video") existing.videoTrack = local.track;
    if (local.kind === "audio") existing.audioTrack = local.track;
    byKey.set(key, existing);
  }

  for (const remote of options.remoteTracks) {
    const key = callTileKey("remote", remote.participantIdentity, tileSourceForTrack(remote.source));
    const existing = byKey.get(key) ?? {
      key,
      identity: remote.participantIdentity,
      label: displayNameForIdentity(remote.participantIdentity),
      isSelf: false,
      mirrorVideo: false,
      micEnabledHint: true,
      videoTrack: null,
      audioTrack: null,
    };
    if (remote.kind === "video") existing.videoTrack = remote.track;
    if (remote.kind === "audio") existing.audioTrack = remote.track;
    byKey.set(key, existing);
  }

  return Array.from(byKey.values());
}
