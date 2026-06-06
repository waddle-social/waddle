import { describe, expect, test } from "bun:test";
import {
  buildCallVolumeMixerRows,
  resetCallVolumeMixerLevels,
  type CallVolumeLevelStore,
} from "../src/lib/calls/call-volume-mixer";

const fakeTrack = {} as never;

describe("call volume mixer projection", () => {
  test("groups each remote participant's voice before screen-share audio and excludes self", () => {
    const rows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: [
        "carol@waddle.test/tablet",
        "alice@waddle.test/web",
        "bob@waddle.test/desktop",
      ],
      remoteTracks: [
        remoteAudio("bob-mic", "bob@waddle.test/desktop", "microphone"),
        remoteAudio("bob-screen", "bob@waddle.test/desktop", "screen_share_audio"),
        remoteAudio("alice-screen", "alice@waddle.test/web", "screen_share_audio"),
        remoteAudio("self-mic", "me@waddle.test/browser", "microphone"),
      ],
      localIdentity: "me@waddle.test/browser",
      levels: {
        "bob@waddle.test/desktop:microphone": 0.4,
        "bob@waddle.test/desktop:screen_share_audio": 0.7,
        "alice@waddle.test/web:screen_share_audio": 0,
      },
    });

    expect(rows.map((row) => ({
      key: row.key,
      label: row.label,
      source: row.source,
      level: row.level,
      disabled: row.disabled,
      hint: row.hint,
      muted: row.muted,
    }))).toEqual([
      {
        key: "carol@waddle.test/tablet:microphone",
        label: "carol",
        source: "microphone",
        level: 1,
        disabled: true,
        hint: "mic off",
        muted: false,
      },
      {
        key: "alice@waddle.test/web:microphone",
        label: "alice",
        source: "microphone",
        level: 1,
        disabled: true,
        hint: "mic off",
        muted: false,
      },
      {
        key: "alice@waddle.test/web:screen_share_audio",
        label: "alice's screen",
        source: "screen_share_audio",
        level: 0,
        disabled: false,
        hint: null,
        muted: true,
      },
      {
        key: "bob@waddle.test/desktop:microphone",
        label: "bob",
        source: "microphone",
        level: 0.4,
        disabled: false,
        hint: null,
        muted: false,
      },
      {
        key: "bob@waddle.test/desktop:screen_share_audio",
        label: "bob's screen",
        source: "screen_share_audio",
        level: 0.7,
        disabled: false,
        hint: null,
        muted: false,
      },
    ]);
  });
});

describe("call volume mixer reducer", () => {
  test("reset all returns every stored participant audio entry to full volume", () => {
    const levels: CallVolumeLevelStore = {
      "alice@waddle.test/web:microphone": 0.2,
      "alice@waddle.test/web:screen_share_audio": 0,
      "bob@waddle.test/desktop:microphone": 0.8,
    };

    expect(resetCallVolumeMixerLevels(levels)).toEqual({
      "alice@waddle.test/web:microphone": 1,
      "alice@waddle.test/web:screen_share_audio": 1,
      "bob@waddle.test/desktop:microphone": 1,
    });
  });
});

function remoteAudio(
  publicationSid: string,
  participantIdentity: string,
  source: "microphone" | "screen_share_audio",
) {
  return {
    participantIdentity,
    publicationSid,
    kind: "audio" as const,
    source,
    track: fakeTrack,
  };
}
