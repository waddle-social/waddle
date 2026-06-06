import { describe, expect, test } from "bun:test";
import { buildCallTiles } from "../src/lib/calls/call-tiles";

const fakeTrack = {} as never;

describe("call tile projection", () => {
  test("projects a remote camera and screen share as distinct tiles", () => {
    const screenAudioTrack = {} as never;
    const tiles = buildCallTiles({
      remoteTracks: [
        {
          participantIdentity: "alice@example.com/web",
          publicationSid: "camera",
          kind: "video",
          source: "camera",
          track: fakeTrack,
        },
        {
          participantIdentity: "alice@example.com/web",
          publicationSid: "screen",
          kind: "video",
          source: "screen_share",
          track: fakeTrack,
        },
        {
          participantIdentity: "alice@example.com/web",
          publicationSid: "screen-audio",
          kind: "audio",
          source: "screen_share_audio",
          track: screenAudioTrack,
        },
      ],
      localTracks: [],
      localIdentity: null,
      micEnabled: true,
    });

    expect(tiles.map((tile) => ({
      key: tile.key,
      label: tile.label,
      source: tile.source,
      isSelf: tile.isSelf,
      mirrorVideo: tile.mirrorVideo,
      showsPresentingGlyph: tile.showsPresentingGlyph,
    }))).toEqual([
      {
        key: "self:you:camera",
        label: "You",
        source: "camera",
        isSelf: true,
        mirrorVideo: true,
        showsPresentingGlyph: false,
      },
      {
        key: "remote:alice@example.com/web:camera",
        label: "alice",
        source: "camera",
        isSelf: false,
        mirrorVideo: false,
        showsPresentingGlyph: false,
      },
      {
        key: "remote:alice@example.com/web:screen_share",
        label: "alice's screen",
        source: "screen_share",
        isSelf: false,
        mirrorVideo: false,
        showsPresentingGlyph: true,
      },
    ]);
    expect(tiles.find((tile) => tile.key === "remote:alice@example.com/web:screen_share")?.audioTrack)
      .toBe(screenAudioTrack);
  });

  test("projects local camera and screen share with separate mirror decisions", () => {
    const tiles = buildCallTiles({
      remoteTracks: [],
      localTracks: [
        {
          participantIdentity: "me@example.com/web",
          publicationSid: "camera",
          kind: "video",
          source: "camera",
          track: fakeTrack,
        },
        {
          participantIdentity: "me@example.com/web",
          publicationSid: "screen",
          kind: "video",
          source: "screen_share",
          track: fakeTrack,
        },
      ],
      localIdentity: "me@example.com/web",
      micEnabled: true,
    });

    expect(tiles.map((tile) => ({
      key: tile.key,
      label: tile.label,
      source: tile.source,
      isSelf: tile.isSelf,
      mirrorVideo: tile.mirrorVideo,
      showsPresentingGlyph: tile.showsPresentingGlyph,
    }))).toEqual([
      {
        key: "self:me@example.com/web:camera",
        label: "You",
        source: "camera",
        isSelf: true,
        mirrorVideo: true,
        showsPresentingGlyph: false,
      },
      {
        key: "self:me@example.com/web:screen_share",
        label: "Your screen",
        source: "screen_share",
        isSelf: true,
        mirrorVideo: false,
        showsPresentingGlyph: true,
      },
    ]);
  });
});
