import { describe, expect, test } from "bun:test";
import { projectCallTiles } from "../src/lib/calls/call-tile-projection";
import type { LocalMediaTrack, RemoteMediaTrack } from "../src/lib/calls/engine";

const fakeTrack = {} as never;

describe("call tile spotlight projection", () => {
  test("promotes a newly appearing remote screen share and records its appear edge", () => {
    const projection = projectCallTiles({
      remoteTracks: [
        remoteVideo("bob@example.com/web", "camera-pub", "camera"),
        remoteVideo("bob@example.com/web", "screen-pub", "screen_share"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(),
      manualFocusKey: null,
    });

    expect(projection.spotlightKey).toBe("remote:bob@example.com/web:screen_share");
    expect(Array.from(projection.seenRemoteScreenTrackKeys)).toEqual(["screen-pub"]);
  });

  test("keeps an already seen remote screen on stage without replaying an appear edge", () => {
    const seenRemoteScreenTrackKeys = new Set(["screen-pub"]);
    const projection = projectCallTiles({
      remoteTracks: [
        remoteVideo("bob@example.com/web", "camera-pub", "camera"),
        remoteVideo("bob@example.com/web", "screen-pub", "screen_share"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys,
      manualFocusKey: null,
    });

    expect(projection.spotlightKey).toBe("remote:bob@example.com/web:screen_share");
    expect(projection.seenRemoteScreenTrackKeys).not.toBe(seenRemoteScreenTrackKeys);
    expect(Array.from(projection.seenRemoteScreenTrackKeys)).toEqual(["screen-pub"]);
  });

  test("lets manual tile focus override a remote screen spotlight", () => {
    const projection = projectCallTiles({
      remoteTracks: [
        remoteVideo("bob@example.com/web", "camera-pub", "camera"),
        remoteVideo("bob@example.com/web", "screen-pub", "screen_share"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(["screen-pub"]),
      manualFocusKey: "remote:bob@example.com/web:camera",
    });

    expect(projection.spotlightKey).toBe("remote:bob@example.com/web:camera");
  });

  test("falls back to another remote screen when the spotlighted screen ends", () => {
    const projection = projectCallTiles({
      remoteTracks: [
        remoteVideo("carol@example.com/web", "screen-pub", "screen_share"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(["bob-screen-pub", "screen-pub"]),
      manualFocusKey: "remote:bob@example.com/web:screen_share",
    });

    expect(projection.spotlightKey).toBe("remote:carol@example.com/web:screen_share");
  });

  test("returns to the equal grid when no remote screens remain", () => {
    const projection = projectCallTiles({
      remoteTracks: [remoteVideo("bob@example.com/web", "camera-pub", "camera")],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(["screen-pub"]),
      manualFocusKey: "remote:bob@example.com/web:screen_share",
    });

    expect(projection.spotlightKey).toBeNull();
  });

  test("does not auto-promote the local participant's own screen share", () => {
    const projection = projectCallTiles({
      remoteTracks: [],
      localTracks: [localVideo("alice@example.com/web", "screen-pub", "screen_share")],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(),
      manualFocusKey: null,
    });

    expect(projection.spotlightKey).toBeNull();
    expect(Array.from(projection.seenRemoteScreenTrackKeys)).toEqual([]);
  });

  test("treats a restarted same-participant screen publication as a new appear edge", () => {
    const projection = projectCallTiles({
      remoteTracks: [
        remoteVideo("carol@example.com/web", "carol-screen-pub", "screen_share"),
        remoteVideo("bob@example.com/web", "bob-screen-pub-2", "screen_share"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set([
        "bob-screen-pub-1",
        "carol-screen-pub",
      ]),
      manualFocusKey: null,
    });

    expect(projection.spotlightKey).toBe("remote:bob@example.com/web:screen_share");
    expect(Array.from(projection.seenRemoteScreenTrackKeys)).toEqual([
      "bob-screen-pub-1",
      "carol-screen-pub",
      "bob-screen-pub-2",
    ]);
  });
});

function remoteVideo(
  participantIdentity: string,
  publicationSid: string,
  source: RemoteMediaTrack["source"],
): RemoteMediaTrack {
  return {
    participantIdentity,
    publicationSid,
    kind: "video",
    source,
    track: fakeTrack,
  };
}

function localVideo(
  participantIdentity: string,
  publicationSid: string,
  source: LocalMediaTrack["source"],
): LocalMediaTrack {
  return {
    participantIdentity,
    publicationSid,
    kind: "video",
    source,
    track: fakeTrack,
  };
}
