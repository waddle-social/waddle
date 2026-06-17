import { describe, expect, test } from "bun:test";
import {
  projectCallTiles,
  reconcileCallTileProjectionState,
  retainManualFocusKey,
} from "../src/lib/calls/call-tile-projection";
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

  test("keeps the newest seen active remote screen on stage after recording its appear edge", () => {
    const projection = projectCallTiles({
      remoteTracks: [
        remoteVideo("carol@example.com/web", "carol-screen-pub", "screen_share"),
        remoteVideo("bob@example.com/web", "bob-screen-pub", "screen_share"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(["carol-screen-pub", "bob-screen-pub"]),
      manualFocusKey: null,
    });

    expect(projection.spotlightKey).toBe("remote:bob@example.com/web:screen_share");
  });

  test("lets a remote screen share outrank a pin for the large tile", () => {
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

    // Screen share always claims the large tile, even over an explicit pin —
    // the pin is retained and re-asserts once the share stops.
    expect(projection.spotlightKey).toBe("remote:bob@example.com/web:screen_share");
  });

  test("honours a pin for the large tile when nothing is being shared", () => {
    const projection = projectCallTiles({
      remoteTracks: [
        remoteVideo("bob@example.com/web", "bob-camera", "camera"),
        remoteVideo("carol@example.com/web", "carol-camera", "camera"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(),
      manualFocusKey: "remote:carol@example.com/web:camera",
    });

    expect(projection.spotlightKey).toBe("remote:carol@example.com/web:camera");
  });

  test("lets a pin override active-speaker promotion in Speaker view", () => {
    const projection = projectCallTiles({
      remoteTracks: [
        remoteVideo("bob@example.com/web", "bob-camera", "camera"),
        remoteVideo("carol@example.com/web", "carol-camera", "camera"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(),
      manualFocusKey: "remote:bob@example.com/web:camera",
      viewMode: "speaker",
      promotedSpeakerIdentity: "carol@example.com/web",
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

  test("uses one remote tile for a DM peer that is publishing only a screen", () => {
    const sharing = projectCallTiles({
      remoteTracks: [remoteVideo("bob@example.com/web", "screen-pub", "screen_share")],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      expectedRemoteIdentities: ["bob@example.com/phone"],
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(),
      manualFocusKey: null,
    });

    expect(sharing.spotlightKey).toBe("remote:bob@example.com/web:screen_share");
    expect(sharing.tiles.map((tile) => tile.key)).toEqual([
      "self:alice@example.com/web:camera",
      "remote:bob@example.com/web:screen_share",
    ]);

    const afterStop = projectCallTiles({
      remoteTracks: [],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      expectedRemoteIdentities: ["bob@example.com/phone"],
      micEnabled: true,
      seenRemoteScreenTrackKeys: sharing.seenRemoteScreenTrackKeys,
      manualFocusKey: sharing.spotlightKey,
    });

    expect(afterStop.spotlightKey).toBeNull();
    expect(afterStop.tiles.map((tile) => tile.key)).toEqual([
      "self:alice@example.com/web:camera",
      "remote:bob@example.com/phone:camera",
    ]);
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

  test("clears stale manual focus before a departed tile key can reapply on reconnect", () => {
    const focused = projectCallTiles({
      remoteTracks: [
        remoteVideo("bob@example.com/web", "camera-pub", "camera"),
        remoteVideo("carol@example.com/web", "carol-screen-pub", "screen_share"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(["carol-screen-pub"]),
      manualFocusKey: "remote:bob@example.com/web:camera",
    });
    // Carol's screen outranks Bob's pin for the large tile, but the pin is
    // still retained internally — until Bob leaves and reconcile clears it.
    expect(focused.spotlightKey).toBe("remote:carol@example.com/web:screen_share");

    const afterBobLeaves = projectCallTiles({
      remoteTracks: [
        remoteVideo("carol@example.com/web", "carol-screen-pub", "screen_share"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(["carol-screen-pub"]),
      manualFocusKey: "remote:bob@example.com/web:camera",
    });
    const reconciled = reconcileCallTileProjectionState({
      tiles: afterBobLeaves.tiles,
      manualFocusKey: "remote:bob@example.com/web:camera",
      currentSeenRemoteScreenTrackKeys: new Set(["carol-screen-pub"]),
      nextSeenRemoteScreenTrackKeys: afterBobLeaves.seenRemoteScreenTrackKeys,
    });

    expect(afterBobLeaves.spotlightKey).toBe("remote:carol@example.com/web:screen_share");
    expect(reconciled.manualFocusKey).toBeNull();

    const afterBobRejoins = projectCallTiles({
      remoteTracks: [
        remoteVideo("carol@example.com/web", "carol-screen-pub", "screen_share"),
        remoteVideo("bob@example.com/web", "camera-pub-2", "camera"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: reconciled.seenRemoteScreenTrackKeys,
      manualFocusKey: reconciled.manualFocusKey,
    });
    expect(afterBobRejoins.spotlightKey).toBe("remote:carol@example.com/web:screen_share");
  });

  test("promotes the active speaker's camera tile in Speaker view", () => {
    const projection = projectCallTiles({
      remoteTracks: [
        remoteVideo("bob@example.com/web", "bob-camera", "camera"),
        remoteVideo("carol@example.com/web", "carol-camera", "camera"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(),
      manualFocusKey: null,
      viewMode: "speaker",
      promotedSpeakerIdentity: "carol@example.com/web",
    });

    expect(projection.spotlightKey).toBe("remote:carol@example.com/web:camera");
  });

  test("keeps the equal grid in Gallery view even with an active speaker", () => {
    const projection = projectCallTiles({
      remoteTracks: [remoteVideo("bob@example.com/web", "bob-camera", "camera")],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(),
      manualFocusKey: null,
      viewMode: "gallery",
      promotedSpeakerIdentity: "bob@example.com/web",
    });

    expect(projection.spotlightKey).toBeNull();
  });

  test("falls back to a remote camera in Speaker view before anyone has spoken", () => {
    const projection = projectCallTiles({
      remoteTracks: [remoteVideo("bob@example.com/web", "bob-camera", "camera")],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(),
      manualFocusKey: null,
      viewMode: "speaker",
      promotedSpeakerIdentity: null,
    });

    expect(projection.spotlightKey).toBe("remote:bob@example.com/web:camera");
  });

  test("falls back to the self tile in Speaker view when alone in the call", () => {
    const projection = projectCallTiles({
      remoteTracks: [],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(),
      manualFocusKey: null,
      viewMode: "speaker",
      promotedSpeakerIdentity: null,
    });

    expect(projection.spotlightKey).toBe("self:alice@example.com/web:camera");
  });

  test("lets a screen share outrank active-speaker promotion in Speaker view", () => {
    const projection = projectCallTiles({
      remoteTracks: [
        remoteVideo("bob@example.com/web", "bob-screen", "screen_share"),
        remoteVideo("carol@example.com/web", "carol-camera", "camera"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(),
      manualFocusKey: null,
      viewMode: "speaker",
      promotedSpeakerIdentity: "carol@example.com/web",
    });

    expect(projection.spotlightKey).toBe("remote:bob@example.com/web:screen_share");
  });

  test("retains manual focus while its target tile is still present", () => {
    const projection = projectCallTiles({
      remoteTracks: [
        remoteVideo("bob@example.com/web", "camera-pub", "camera"),
        remoteVideo("carol@example.com/web", "carol-screen-pub", "screen_share"),
      ],
      localTracks: [],
      localIdentity: "alice@example.com/web",
      micEnabled: true,
      seenRemoteScreenTrackKeys: new Set(["carol-screen-pub"]),
      manualFocusKey: "remote:bob@example.com/web:camera",
    });

    // Carol's screen holds the large tile, yet the pin on Bob's still-present
    // camera tile is retained (it re-asserts once Carol stops sharing).
    expect(projection.spotlightKey).toBe("remote:carol@example.com/web:screen_share");
    expect(retainManualFocusKey(projection.tiles, "remote:bob@example.com/web:camera"))
      .toBe("remote:bob@example.com/web:camera");
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
