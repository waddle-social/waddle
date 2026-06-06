import { describe, expect, test } from "bun:test";
import { localScreenSharePresentation } from "../src/lib/calls/call-self-share";
import type { LocalMediaTrack } from "../src/lib/calls/engine";

const fakeTrack = {} as LocalMediaTrack["track"];

function localVideo(
  publicationSid: string,
  source: LocalMediaTrack["source"],
): LocalMediaTrack {
  return {
    participantIdentity: "alice@waddle.test/web",
    publicationSid,
    kind: "video",
    source,
    track: fakeTrack,
  };
}

function localAudio(
  publicationSid: string,
  source: LocalMediaTrack["source"],
): LocalMediaTrack {
  return {
    participantIdentity: "alice@waddle.test/web",
    publicationSid,
    kind: "audio",
    source,
    track: fakeTrack,
  };
}

describe("local screen-share presentation", () => {
  test("stays absent when screen sharing is not active", () => {
    expect(localScreenSharePresentation({
      screenShareEnabled: false,
      localTracks: [
        localVideo("screen-pub", "screen_share"),
      ],
    })).toBeNull();
  });

  test("stays absent while screen sharing is enabled before the screen track publishes", () => {
    expect(localScreenSharePresentation({
      screenShareEnabled: true,
      localTracks: [],
    })).toBeNull();
  });

  test("shows a presenter banner and self-thumbnail while keeping that track out of the stage", () => {
    const camera = localVideo("camera-pub", "camera");
    const screen = localVideo("screen-pub", "screen_share");
    const screenAudio = localAudio("screen-audio-pub", "screen_share_audio");
    const presentation = localScreenSharePresentation({
      screenShareEnabled: true,
      localTracks: [
        camera,
        screen,
        screenAudio,
      ],
    });

    expect(presentation).toEqual({
      message: "You're sharing your screen",
      thumbnail: {
        attachKey: "self:alice@waddle.test/web:screen_share",
        label: "Your screen",
        mirrorVideo: false,
        videoTrack: fakeTrack,
      },
      stageLocalTracks: [camera],
    });
  });
});
