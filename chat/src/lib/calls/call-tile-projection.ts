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
    ?? remoteScreens[0]?.key
    ?? null;

  return {
    tiles,
    spotlightKey,
    seenRemoteScreenTrackKeys: nextSeenRemoteScreenTrackKeys,
  };
}
