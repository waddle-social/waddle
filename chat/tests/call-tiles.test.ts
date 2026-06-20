import { describe, expect, test } from "bun:test";
import { buildCallTiles } from "../src/lib/calls/call-tiles";
import type { CallTrackSource, LocalMediaTrack, RemoteMediaTrack } from "../src/lib/calls/engine";

const fakeTrack = {} as never;

function remoteTrack(
  publicationSid: string,
  participantIdentity: string,
  kind: "audio" | "video",
  source: CallTrackSource,
): RemoteMediaTrack {
  return { participantIdentity, publicationSid, kind, source, track: fakeTrack };
}

function localTrack(
  participantIdentity: string,
  kind: "audio" | "video",
  source: CallTrackSource,
): LocalMediaTrack {
  return { participantIdentity, kind, source, track: fakeTrack, publicationSid: "self-pub" };
}

describe("buildCallTiles micEnabledHint", () => {
  test("remote tile mic hint reflects presence mute, not a hard-coded true (#1030)", () => {
    const tiles = buildCallTiles({
      remoteTracks: [
        remoteTrack("alice-cam", "alice@waddle.test/web", "video", "camera"),
        remoteTrack("bob-cam", "bob@waddle.test/desktop", "video", "camera"),
      ],
      localTracks: [localTrack("me@waddle.test/browser", "video", "camera")],
      localIdentity: "me@waddle.test/browser",
      micEnabled: true,
      mutedKeys: new Set<string>(["alice@waddle.test/web"]),
    });

    const alice = tiles.find((tile) => tile.identity === "alice@waddle.test/web" && tile.source === "camera");
    const bob = tiles.find((tile) => tile.identity === "bob@waddle.test/desktop" && tile.source === "camera");
    expect(alice?.micEnabledHint).toBe(false);
    expect(bob?.micEnabledHint).toBe(true);
  });

  test("self tile mic hint reflects the local mic flag regardless of mutedKeys", () => {
    const tiles = buildCallTiles({
      remoteTracks: [],
      localTracks: [localTrack("me@waddle.test/browser", "video", "camera")],
      localIdentity: "me@waddle.test/browser",
      micEnabled: false,
      // Even if our own identity leaked into mutedKeys, self uses micEnabled.
      mutedKeys: new Set<string>(["me@waddle.test/browser"]),
    });
    const self = tiles.find((tile) => tile.isSelf && tile.source === "camera");
    expect(self?.micEnabledHint).toBe(false);
  });

  test("a muted owner's screen-share tile keeps micEnabledHint true (#1030)", () => {
    // A presenter who mutes their mic while sharing their screen must NOT
    // get a mic-off badge on the screen-share tile — mute is a mic concept,
    // not a screen concept. Only their camera tile reflects the mute.
    const tiles = buildCallTiles({
      remoteTracks: [
        remoteTrack("alice-cam", "alice@waddle.test/web", "video", "camera"),
        remoteTrack("alice-screen", "alice@waddle.test/web", "video", "screen_share"),
      ],
      localTracks: [],
      localIdentity: "me@waddle.test/browser",
      micEnabled: true,
      mutedKeys: new Set<string>(["alice@waddle.test/web"]),
    });
    const camera = tiles.find(
      (tile) => tile.identity === "alice@waddle.test/web" && tile.source === "camera",
    );
    const screen = tiles.find(
      (tile) => tile.identity === "alice@waddle.test/web" && tile.source === "screen_share",
    );
    expect(camera?.micEnabledHint).toBe(false); // muted → camera tile shows it
    expect(screen?.micEnabledHint).toBe(true); // screen tile never shows mute
  });

  test("defaults remote mic hint to enabled when no mutedKeys given", () => {
    const tiles = buildCallTiles({
      remoteTracks: [remoteTrack("alice-cam", "alice@waddle.test/web", "video", "camera")],
      localTracks: [],
      localIdentity: "me@waddle.test/browser",
      micEnabled: true,
    });
    const alice = tiles.find((tile) => tile.identity === "alice@waddle.test/web");
    expect(alice?.micEnabledHint).toBe(true);
  });
});
