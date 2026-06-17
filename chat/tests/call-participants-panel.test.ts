import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import { buildCallRoster } from "../src/lib/calls/call-roster";
import { buildCallVolumeMixerRows } from "../src/lib/calls/call-volume-mixer";
import type { CallTrackSource, RemoteMediaTrack } from "../src/lib/calls/engine";

const fakeTrack = {} as never;

function remoteTrack(
  publicationSid: string,
  participantIdentity: string,
  kind: "audio" | "video",
  source: CallTrackSource,
): RemoteMediaTrack {
  return { participantIdentity, publicationSid, kind, source, track: fakeTrack };
}

function rosterFixture() {
  const identities = ["alice@waddle.test/web", "bob@waddle.test/desktop"];
  const tracks = [
    remoteTrack("alice-mic", "alice@waddle.test/web", "audio", "microphone"),
    remoteTrack("alice-cam", "alice@waddle.test/web", "video", "camera"),
  ];
  const volumeRows = buildCallVolumeMixerRows({
    remoteParticipantIdentities: identities,
    remoteTracks: tracks,
    localIdentity: "me@waddle.test/browser",
    levels: { "alice@waddle.test/web:microphone": 1.5 },
  });
  return buildCallRoster({
    remoteParticipantIdentities: identities,
    remoteTracks: tracks,
    localIdentity: "me@waddle.test/browser",
    localMicEnabled: true,
    localCameraEnabled: false,
    activeSpeakerIdentities: new Set<string>(["alice@waddle.test/web"]),
    volumeRows,
  });
}

describe("CallParticipantsPanel", () => {
  test("lists every attendee with live mic/camera state, speaking, and per-row volume", async () => {
    const html = await renderVueComponent(
      "../src/components/calls/CallParticipantsPanel.vue",
      { rows: rosterFixture() },
      import.meta.url,
    );

    // Every attendee is listed, self first.
    expect(html).toContain("You");
    expect(html).toContain("alice");
    expect(html).toContain("bob");

    // Live mic + camera state is exposed accessibly.
    expect(html).toContain('aria-label="Microphone on"');
    expect(html).toContain('aria-label="Microphone off"');
    expect(html).toContain('aria-label="Camera on"');
    expect(html).toContain('aria-label="Camera off"');

    // The active speaker is flagged.
    expect(html).toContain('aria-label="Speaking"');

    // A working per-participant volume slider, reflecting the stored level.
    expect(html).toContain('aria-label="Volume for alice"');
    expect(html).toContain('aria-valuetext="150%"');
    expect(html).toContain('max="200"');

    // bob's mic is off, so his volume row is disabled with a hint.
    expect(html).toContain("mic off");
    expect(html).toContain("disabled");

    expect(html).toContain("Reset all");
  });

  test("the slider snaps gains via callVolumePercentToGain and emits setVolume/resetAll", () => {
    const source = readFileSync(
      new URL("../src/components/calls/CallParticipantsPanel.vue", import.meta.url),
      "utf8",
    );
    expect(source).toContain("callVolumePercentToGain");
    expect(source).toContain('emit("setVolume"');
    expect(source).toContain('emit("resetAll")');
  });
});
