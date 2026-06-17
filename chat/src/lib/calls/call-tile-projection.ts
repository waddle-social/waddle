import { buildCallTiles, type BuildCallTilesInput, type CallTileModel } from "./call-tiles";
import type { CallViewMode } from "./view-mode";

export type ProjectCallTilesInput = BuildCallTilesInput & {
  seenRemoteScreenTrackKeys: ReadonlySet<string>;
  manualFocusKey: string | null;
  /** Stage layout the user has chosen for this call. Defaults to `gallery`. */
  viewMode?: CallViewMode;
  /** Identity the Speaker layout should auto-promote to the large tile. */
  promotedSpeakerIdentity?: string | null;
};

export type CallTileProjection = {
  tiles: CallTileModel[];
  spotlightKey: string | null;
  seenRemoteScreenTrackKeys: ReadonlySet<string>;
};

export type ReconcileCallTileProjectionStateInput = {
  tiles: readonly CallTileModel[];
  manualFocusKey: string | null;
  currentSeenRemoteScreenTrackKeys: ReadonlySet<string>;
  nextSeenRemoteScreenTrackKeys: ReadonlySet<string>;
};

export type ReconciledCallTileProjectionState = {
  manualFocusKey: string | null;
  seenRemoteScreenTrackKeys: ReadonlySet<string>;
};

export function reconcileCallTileProjectionState(
  input: ReconcileCallTileProjectionStateInput,
): ReconciledCallTileProjectionState {
  return {
    manualFocusKey: retainManualFocusKey(input.tiles, input.manualFocusKey),
    seenRemoteScreenTrackKeys: sameSet(
      input.currentSeenRemoteScreenTrackKeys,
      input.nextSeenRemoteScreenTrackKeys,
    )
      ? input.currentSeenRemoteScreenTrackKeys
      : input.nextSeenRemoteScreenTrackKeys,
  };
}

export function retainManualFocusKey(
  tiles: readonly CallTileModel[],
  manualFocusKey: string | null,
): string | null {
  if (manualFocusKey === null) return null;
  return tiles.some((tile) => tile.key === manualFocusKey) ? manualFocusKey : null;
}

export function projectCallTiles(input: ProjectCallTilesInput): CallTileProjection {
  const tiles = buildCallTiles(input);
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

  const manualTile = input.manualFocusKey
    ? tiles.find((tile) => tile.key === input.manualFocusKey) ?? null
    : null;
  // Precedence: a screen share always claims the large tile, then a local
  // pin, then (in Speaker view) the auto-promoted active speaker; Gallery
  // with nothing sharing or pinned falls through to the equal grid.
  const spotlightKey = newlyAppearedScreen?.key
    ?? newestSeenRemoteScreen(remoteScreens, nextSeenRemoteScreenTrackKeys)?.key
    ?? manualTile?.key
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
