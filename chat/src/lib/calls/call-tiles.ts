import type { CallTrackSource, LocalMediaTrack, RemoteMediaTrack } from "./engine";
import type { TileAttachable } from "./tile-attach";
import type { CallState } from "./types";
import { barePeerJid } from "../xmpp/jid";

type TileSource = "camera" | "screen_share";

export type CallTileModel = {
  key: string;
  identity: string;
  label: string;
  source: TileSource;
  screenTrackKey: string | null;
  isSelf: boolean;
  mirrorVideo: boolean;
  showsPresentingGlyph: boolean;
  micEnabledHint: boolean;
  videoTrack: TileAttachable | null;
};

export type BuildCallTilesInput = {
  remoteTracks: readonly RemoteMediaTrack[];
  localTracks: readonly LocalMediaTrack[];
  localIdentity: string | null;
  expectedRemoteIdentities?: readonly string[];
  micEnabled: boolean;
};

function displayNameForIdentity(identity: string): string {
  const at = identity.indexOf("@");
  if (at <= 0) return identity;
  return identity.slice(0, at);
}

function tileSourceForTrack(source: CallTrackSource): TileSource {
  return source === "screen_share" || source === "screen_share_audio"
    ? "screen_share"
    : "camera";
}

function labelFor(identity: string, isSelf: boolean, source: TileSource): string {
  if (source === "screen_share") return isSelf ? "Your screen" : `${displayNameForIdentity(identity)}'s screen`;
  return isSelf ? "You" : displayNameForIdentity(identity);
}

function callTileKey(side: "self" | "remote", identity: string, source: TileSource): string {
  return `${side}:${identity}:${source}`;
}

function bareIdentityKey(identity: string): string {
  return barePeerJid(identity.trim()).toLowerCase();
}

export function expectedRemoteIdentitiesForCallState(state: CallState): string[] {
  if (state.phase !== "active" || state.kind !== "dm") return [];
  const peer = state.peer.trim();
  return bareIdentityKey(peer) ? [peer] : [];
}

function newTile(
  side: "self" | "remote",
  identity: string,
  source: TileSource,
  micEnabled: boolean,
): CallTileModel {
  const isSelf = side === "self";
  return {
    key: callTileKey(side, identity, source),
    identity,
    label: labelFor(identity, isSelf, source),
    source,
    screenTrackKey: null,
    isSelf,
    mirrorVideo: isSelf && source === "camera",
    showsPresentingGlyph: source === "screen_share",
    micEnabledHint: isSelf ? micEnabled : true,
    videoTrack: null,
  };
}

export function buildCallTiles(input: BuildCallTilesInput): CallTileModel[] {
  const byKey = new Map<string, CallTileModel>();
  const activeRemoteBareIdentities = new Set<string>();
  const selfId = input.localTracks[0]?.participantIdentity ?? input.localIdentity ?? "you";
  byKey.set(callTileKey("self", selfId, "camera"), newTile("self", selfId, "camera", input.micEnabled));

  for (const local of input.localTracks) {
    const source = tileSourceForTrack(local.source);
    const key = callTileKey("self", local.participantIdentity, source);
    const existing = byKey.get(key) ?? newTile("self", local.participantIdentity, source, input.micEnabled);
    if (local.kind === "video") {
      existing.videoTrack = local.track;
      if (source === "screen_share") existing.screenTrackKey = local.publicationSid;
    }
    byKey.set(key, existing);
  }

  for (const remote of input.remoteTracks) {
    const participantIdentity = remote.participantIdentity.trim();
    if (!participantIdentity) continue;
    const remoteBareIdentity = bareIdentityKey(participantIdentity);
    if (remoteBareIdentity) activeRemoteBareIdentities.add(remoteBareIdentity);
    const source = tileSourceForTrack(remote.source);
    const key = callTileKey("remote", participantIdentity, source);
    const existing = byKey.get(key) ?? newTile("remote", participantIdentity, source, input.micEnabled);
    if (remote.kind === "video") {
      existing.videoTrack = remote.track;
      if (source === "screen_share") existing.screenTrackKey = remote.publicationSid;
    }
    byKey.set(key, existing);
  }

  for (const identity of input.expectedRemoteIdentities ?? []) {
    const trimmedIdentity = identity.trim();
    const bareIdentity = bareIdentityKey(trimmedIdentity);
    if (!bareIdentity || activeRemoteBareIdentities.has(bareIdentity)) continue;
    byKey.set(
      callTileKey("remote", trimmedIdentity, "camera"),
      newTile("remote", trimmedIdentity, "camera", input.micEnabled),
    );
  }

  return Array.from(byKey.values());
}
