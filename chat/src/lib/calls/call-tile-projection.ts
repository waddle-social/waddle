import { buildCallTiles, type BuildCallTilesInput, type CallTileModel } from "./call-tiles";
import type { CallViewMode } from "./view-mode";

export type ProjectCallTilesInput = BuildCallTilesInput & {
  seenRemoteScreenTrackKeys: ReadonlySet<string>;
  pinnedTileKey: string | null;
  /** Stage layout the user has chosen for this call. Defaults to `gallery`. */
  viewMode?: CallViewMode;
  /** Identity the Speaker layout should auto-promote to the large tile. */
  promotedSpeakerIdentity?: string | null;
  /**
   * Drop the local participant's own self-view tile from the stage. Purely a
   * local rendering choice — the camera keeps publishing to other participants.
   * Removing the self tile before spotlight selection also means the local
   * participant is never auto-promoted to the large Speaker tile while hidden.
   */
  hideSelfView?: boolean;
};

export type CallTileProjection = {
  tiles: CallTileModel[];
  spotlightKey: string | null;
  seenRemoteScreenTrackKeys: ReadonlySet<string>;
};

export type ReconcileCallTileProjectionStateInput = {
  tiles: readonly CallTileModel[];
  pinnedTileKey: string | null;
  currentSeenRemoteScreenTrackKeys: ReadonlySet<string>;
  nextSeenRemoteScreenTrackKeys: ReadonlySet<string>;
};

export type ReconciledCallTileProjectionState = {
  pinnedTileKey: string | null;
  seenRemoteScreenTrackKeys: ReadonlySet<string>;
};

export function reconcileCallTileProjectionState(
  input: ReconcileCallTileProjectionStateInput,
): ReconciledCallTileProjectionState {
  return {
    pinnedTileKey: retainPinnedTileKey(input.tiles, input.pinnedTileKey),
    seenRemoteScreenTrackKeys: sameSet(
      input.currentSeenRemoteScreenTrackKeys,
      input.nextSeenRemoteScreenTrackKeys,
    )
      ? input.currentSeenRemoteScreenTrackKeys
      : input.nextSeenRemoteScreenTrackKeys,
  };
}

export function retainPinnedTileKey(
  tiles: readonly CallTileModel[],
  pinnedTileKey: string | null,
): string | null {
  if (pinnedTileKey === null) return null;
  return tiles.some((tile) => tile.key === pinnedTileKey) ? pinnedTileKey : null;
}

export function projectCallTiles(input: ProjectCallTilesInput): CallTileProjection {
  const built = buildCallTiles(input);
  const tiles = input.hideSelfView ? built.filter((tile) => !tile.isSelf) : built;
  const remoteScreens = tiles.filter((tile) =>
    tile.source === "screen_share" && !tile.isSelf && tile.screenTrackKey !== null
  );
  const nextSeenRemoteScreenTrackKeys = new Set(input.seenRemoteScreenTrackKeys);
  const newlyAppearedScreen = remoteScreens.find((tile) =>
    tile.screenTrackKey !== null && !input.seenRemoteScreenTrackKeys.has(tile.screenTrackKey)
  ) ?? null;

  for (const tile of remoteScreens) {
    if (tile.screenTrackKey !== null) nextSeenRemoteScreenTrackKeys.add(tile.screenTrackKey);
  }

  const pinnedTile = input.pinnedTileKey
    ? tiles.find((tile) => tile.key === input.pinnedTileKey) ?? null
    : null;
  // Precedence: an incoming (remote) screen share always claims the large
  // tile, then a local pin, then (in Speaker view) the auto-promoted active
  // speaker; Gallery with nothing sharing or pinned falls through to the equal
  // grid. A local screen share is shown as a self-share notice, not a stage
  // tile, so it never competes here.
  const spotlightKey = newlyAppearedScreen?.key
    ?? newestSeenRemoteScreen(remoteScreens, nextSeenRemoteScreenTrackKeys)?.key
    ?? pinnedTile?.key
    ?? speakerPromotionKey(tiles, input.viewMode ?? "gallery", input.promotedSpeakerIdentity ?? null)
    ?? null;

  return {
    tiles,
    spotlightKey,
    seenRemoteScreenTrackKeys: nextSeenRemoteScreenTrackKeys,
  };
}

/**
 * The large tile in Speaker view: the promoted speaker's camera tile, or — when
 * nobody has spoken yet or the promoted participant has left — a stable
 * fallback (a remote camera tile if any, else the first camera tile) so the
 * Speaker layout always has a large tile. Returns `null` in Gallery view.
 */
function speakerPromotionKey(
  tiles: readonly CallTileModel[],
  viewMode: CallViewMode,
  promotedSpeakerIdentity: string | null,
): string | null {
  if (viewMode !== "speaker") return null;
  const cameraTiles = tiles.filter((tile) => tile.source === "camera");
  const promoted = promotedSpeakerIdentity
    ? cameraTiles.find((tile) => tile.identity === promotedSpeakerIdentity) ?? null
    : null;
  const fallback = cameraTiles.find((tile) => !tile.isSelf) ?? cameraTiles[0] ?? null;
  return (promoted ?? fallback)?.key ?? null;
}

function newestSeenRemoteScreen(
  remoteScreens: readonly CallTileModel[],
  seenRemoteScreenTrackKeys: ReadonlySet<string>,
): CallTileModel | null {
  const activeScreensByTrackKey = new Map(
    remoteScreens.flatMap((tile) => tile.screenTrackKey === null ? [] : [[tile.screenTrackKey, tile]]),
  );
  const newestSeenTrackKeys = Array.from(seenRemoteScreenTrackKeys).reverse();
  for (const screenTrackKey of newestSeenTrackKeys) {
    const tile = activeScreensByTrackKey.get(screenTrackKey);
    if (tile) return tile;
  }
  return remoteScreens[0] ?? null;
}

function sameSet(left: ReadonlySet<string>, right: ReadonlySet<string>): boolean {
  if (left.size !== right.size) return false;
  for (const value of left) {
    if (!right.has(value)) return false;
  }
  return true;
}
