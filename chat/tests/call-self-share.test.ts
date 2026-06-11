import { afterEach, describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import { $callState, clearCallState } from "../src/lib/calls/call-store";
import {
  $callScreenShareEnabled,
  resetCallControls,
} from "../src/lib/calls/call-controls";
import { $callUiMode } from "../src/lib/calls/ui-mode";
import { localScreenSharePresentation } from "../src/lib/calls/call-self-share";
import type { LocalMediaTrack } from "../src/lib/calls/engine";
import { useCallEngine } from "../src/lib/calls/use-call-engine";
import type { LiveKitJoin } from "../src/lib/calls/types";

const fakeTrack = {} as LocalMediaTrack["track"];
const join: LiveKitJoin = {
  url: "wss://livekit.test",
  room: "dm-call",
  identity: "alice@waddle.test/web",
  token: "jwt",
};

afterEach(() => {
  clearCallState();
  resetCallControls(true, true);
  $callScreenShareEnabled.set(false);
  $callUiMode.set("split");
  useCallEngine().localTracks.value = [];
});

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

  test("DM split surface renders the self-share notice while the local screen is published", async () => {
    const camera = localVideo("camera-pub", "camera");
    const screen = localVideo("screen-pub", "screen_share");
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "dm-call",
      media: { audio: true, video: true },
      join,
      kind: "dm",
    });
    $callUiMode.set("split");
    $callScreenShareEnabled.set(true);
    useCallEngine().localTracks.value = [camera, screen];

    const html = await renderVueComponent(
      "../src/components/calls/CallSplitContainer.vue",
      { dmPeerJid: "bob@waddle.test", dmPeerName: "Bob" },
      import.meta.url,
    );

    expect(html).toContain("You&#39;re sharing your screen");
    expect(html).toContain("Your screen");
  });
});
