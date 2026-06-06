import { buildCallTiles, type BuildCallTilesInput, type CallTileModel } from "./call-tiles";

export type ProjectCallTilesInput = BuildCallTilesInput & {
  seenRemoteScreenTrackKeys: ReadonlySet<string>;
  manualFocusKey: string | null;
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
  const spotlightKey = manualTile?.key
    ?? newlyAppearedScreen?.key
    ?? newestSeenRemoteScreen(remoteScreens, nextSeenRemoteScreenTrackKeys)?.key
    ?? null;

  return {
    tiles,
    spotlightKey,
    seenRemoteScreenTrackKeys: nextSeenRemoteScreenTrackKeys,
  };
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
