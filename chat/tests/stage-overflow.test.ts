import { describe, expect, test } from "bun:test";
import type { CallTileModel } from "../src/lib/calls/call-tiles";
import { partitionStageTiles } from "../src/lib/calls/stage-overflow";
import { callAudioSinkTracks } from "../src/lib/calls/call-audio-sink";
import type { RemoteMediaTrack } from "../src/lib/calls/engine";

/**
 * Build a camera tile for `identity`. Only the fields the overflow
 * partition reads (`key`, `identity`) carry meaning; the rest mirror a
 * plain remote camera tile so the fixture is a real `CallTileModel`.
 */
function cameraTile(identity: string): CallTileModel {
  return {
    key: `remote:${identity}:camera`,
    identity,
    label: identity,
    source: "camera",
    screenTrackKey: null,
    isSelf: false,
    mirrorVideo: false,
    showsPresentingGlyph: false,
    micEnabledHint: true,
    videoTrack: null,
  };
}

function cameraTiles(count: number): CallTileModel[] {
  return Array.from({ length: count }, (_, i) => cameraTile(`p${i}@waddle.test/web`));
}

/** A screen-share tile for `identity` (a second tile for the same person). */
function screenTile(identity: string): CallTileModel {
  return {
    ...cameraTile(identity),
    key: `remote:${identity}:screen_share`,
    source: "screen_share",
    screenTrackKey: `${identity}-screen`,
    showsPresentingGlyph: true,
  };
}

describe("partitionStageTiles", () => {
  test("shows every tile and no overflow when the count is under capacity", () => {
    const tiles = cameraTiles(6);
    const partition = partitionStageTiles(tiles, 25);
    expect(partition.tiles).toEqual(tiles);
    expect(partition.overflow).toBeNull();
  });

  test("reserves the last cell for overflow when the count exceeds capacity", () => {
    // 30 distinct people, capacity 25: show 24 real tiles + the overflow
    // cell, which counts the 6 people who have no tile on the Stage.
    const tiles = cameraTiles(30);
    const partition = partitionStageTiles(tiles, 25);
    expect(partition.tiles).toEqual(tiles.slice(0, 24));
    expect(partition.overflow).toEqual({ hiddenCount: 6 });
  });

  test("does not count a person whose camera is on stage but screen-share spilled over", () => {
    const a = cameraTile("a@waddle.test/web");
    const b = cameraTile("b@waddle.test/web");
    const c = cameraTile("c@waddle.test/web");
    const aScreen = screenTile("a@waddle.test/web");
    // capacity 3: show 2 real tiles (a, b); the hidden tail is [c, a-screen].
    // Only `c` is fully off-stage — `a` is still visible via their camera.
    const partition = partitionStageTiles([a, b, c, aScreen], 3);
    expect(partition.tiles).toEqual([a, b]);
    expect(partition.overflow).toEqual({ hiddenCount: 1 });
  });

  test("fills the last cell instead of a '+0 more' tile when no people are hidden", () => {
    const a = cameraTile("a@waddle.test/web");
    const b = cameraTile("b@waddle.test/web");
    const aScreen = screenTile("a@waddle.test/web");
    const bScreen = screenTile("b@waddle.test/web");
    // capacity 3, 4 tiles, but the 2 spilled tiles are just screen-shares of
    // the 2 people already shown — nobody is actually hidden, so there is no
    // overflow and the last cell shows a real tile rather than "+0 more".
    const partition = partitionStageTiles([a, b, aScreen, bScreen], 3);
    expect(partition.tiles).toEqual([a, b, aScreen]);
    expect(partition.overflow).toBeNull();
  });

  test("keeps the single tile and shows no overflow in a degenerate 1-cell Stage", () => {
    // A 1×1 fallback (call dragged tiny) has no room for the affordance, so we
    // keep the one tile rather than render an all-button, no-video Stage.
    // Everyone else stays reachable via the roster/participant count.
    const tiles = cameraTiles(8);
    const partition = partitionStageTiles(tiles, 1);
    expect(partition.tiles).toEqual([tiles[0]]);
    expect(partition.overflow).toBeNull();
  });

  test("shows everyone with no overflow when the count exactly equals capacity", () => {
    const tiles = cameraTiles(25);
    const partition = partitionStageTiles(tiles, 25);
    expect(partition.tiles).toEqual(tiles);
    expect(partition.overflow).toBeNull();
  });

  test("normalizes a fractional capacity down to whole cells", () => {
    // A 2.9-cell Stage is a 2-cell Stage: 1 real tile + overflow for the rest.
    const tiles = cameraTiles(5);
    const partition = partitionStageTiles(tiles, 2.9);
    expect(partition.tiles).toEqual(tiles.slice(0, 1));
    expect(partition.overflow).toEqual({ hiddenCount: 4 });
  });
});

describe("audio is independent of the Stage overflow", () => {
  function remoteCamera(identity: string): RemoteMediaTrack {
    return {
      participantIdentity: identity,
      publicationSid: `${identity}-cam`,
      kind: "video",
      source: "camera",
      track: {} as RemoteMediaTrack["track"],
    };
  }

  function remoteMic(identity: string): RemoteMediaTrack {
    return {
      participantIdentity: identity,
      publicationSid: `${identity}-mic`,
      kind: "audio",
      source: "microphone",
      track: {} as RemoteMediaTrack["track"],
    };
  }

  function sinkIdentitySet(tracks: readonly RemoteMediaTrack[]): Set<string> {
    return new Set(callAudioSinkTracks(tracks).map((track) => track.participantIdentity));
  }

  test("the audio sink set is invariant to Stage capacity, so off-stage people stay audible", () => {
    // 30 people, each publishing a camera and a mic. The audio sink is derived
    // from the raw remote tracks (CallAudioSink), never from the tile grid, so
    // tightening the Stage capacity changes who is on-stage but NOT who is
    // audible. We prove independence by shrinking capacity and showing the sink
    // set is unchanged and still covers everyone the Stage hides.
    const identities = Array.from({ length: 30 }, (_, i) => `p${i}@waddle.test/web`);
    const tiles = identities.map(cameraTile);
    const remoteTracks = identities.flatMap((id) => [remoteCamera(id), remoteMic(id)]);

    const roomy = partitionStageTiles(tiles, 25); // 24 on stage, 6 hidden
    const cramped = partitionStageTiles(tiles, 3); // 2 on stage, 28 hidden
    expect(roomy.overflow).toEqual({ hiddenCount: 6 });
    expect(cramped.overflow).toEqual({ hiddenCount: 28 });

    // CallAudioSink filters mic/screen-audio off the full track list itself —
    // feeding it the same tracks yields the same 30 mics regardless of layout.
    const sinks = sinkIdentitySet(remoteTracks);
    expect(sinks).toEqual(new Set(identities));

    for (const partition of [roomy, cramped]) {
      const onStage = new Set(partition.tiles.map((tile) => tile.identity));
      const offStage = identities.filter((identity) => !onStage.has(identity));
      expect(offStage.length).toBeGreaterThan(0);
      for (const identity of offStage) {
        expect(sinks.has(identity)).toBe(true);
      }
    }
  });
});
