import { describe, expect, test } from "bun:test";
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

describe("buildCallRoster", () => {
  test("lists self first as You, then each remote attendee in order", () => {
    const rows = buildCallRoster({
      remoteParticipantIdentities: [
        "alice@waddle.test/web",
        "bob@waddle.test/desktop",
      ],
      remoteTracks: [],
      localIdentity: "me@waddle.test/browser",
      localMicEnabled: true,
      localCameraEnabled: false,
      activeSpeakerIdentities: new Set<string>(),
      volumeRows: [],
    });

    expect(rows.map((row) => ({ label: row.label, isSelf: row.isSelf }))).toEqual([
      { label: "You", isSelf: true },
      { label: "alice", isSelf: false },
      { label: "bob", isSelf: false },
    ]);
  });

  test("derives mic and camera state from each remote participant's tracks", () => {
    const rows = buildCallRoster({
      remoteParticipantIdentities: [
        "alice@waddle.test/web",
        "bob@waddle.test/desktop",
        "carol@waddle.test/tablet",
      ],
      remoteTracks: [
        remoteTrack("alice-mic", "alice@waddle.test/web", "audio", "microphone"),
        remoteTrack("alice-cam", "alice@waddle.test/web", "video", "camera"),
        // A screen share is neither "mic on" nor "camera on".
        remoteTrack("bob-screen", "bob@waddle.test/desktop", "video", "screen_share"),
      ],
      localIdentity: "me@waddle.test/browser",
      localMicEnabled: true,
      localCameraEnabled: false,
      activeSpeakerIdentities: new Set<string>(),
      volumeRows: [],
    });

    const byLabel = Object.fromEntries(
      rows.map((row) => [row.label, { micOn: row.micOn, cameraOn: row.cameraOn }]),
    );
    expect(byLabel.alice).toEqual({ micOn: true, cameraOn: true });
    expect(byLabel.bob).toEqual({ micOn: false, cameraOn: false });
    expect(byLabel.carol).toEqual({ micOn: false, cameraOn: false });
  });

  test("self mic and camera reflect the local flags, not self tracks", () => {
    const rows = buildCallRoster({
      remoteParticipantIdentities: [],
      // A self mic track must not be counted as a remote attendee, nor
      // override the local flags.
      remoteTracks: [
        remoteTrack("self-mic", "me@waddle.test/browser", "audio", "microphone"),
      ],
      localIdentity: "me@waddle.test/browser",
      localMicEnabled: false,
      localCameraEnabled: true,
      activeSpeakerIdentities: new Set<string>(),
      volumeRows: [],
    });

    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ isSelf: true, micOn: false, cameraOn: true });
  });

  test("marks attendees in the active-speaker set as speaking, normalizing identities", () => {
    const rows = buildCallRoster({
      remoteParticipantIdentities: [
        "alice@waddle.test/web",
        "bob@waddle.test/desktop",
      ],
      remoteTracks: [],
      localIdentity: "me@waddle.test/browser",
      localMicEnabled: true,
      localCameraEnabled: true,
      // The active set may carry a differently-cased bare JID; matching is
      // by identity key, and includes self (LiveKit's held set has local).
      activeSpeakerIdentities: new Set<string>([
        "ALICE@waddle.test/web",
        "me@waddle.test/browser",
      ]),
      volumeRows: [],
    });

    const byLabel = Object.fromEntries(rows.map((row) => [row.label, row.speaking]));
    expect(byLabel.You).toBe(true);
    expect(byLabel.alice).toBe(true);
    expect(byLabel.bob).toBe(false);
  });

  test("groups each remote participant's volume rows onto their row; self has none", () => {
    const tracks = [
      remoteTrack("alice-mic", "alice@waddle.test/web", "audio", "microphone"),
      remoteTrack("alice-screen", "alice@waddle.test/web", "audio", "screen_share_audio"),
      remoteTrack("bob-mic", "bob@waddle.test/desktop", "audio", "microphone"),
    ];
    const identities = ["alice@waddle.test/web", "bob@waddle.test/desktop"];
    const volumeRows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: identities,
      remoteTracks: tracks,
      localIdentity: "me@waddle.test/browser",
      levels: {},
    });

    const rows = buildCallRoster({
      remoteParticipantIdentities: identities,
      remoteTracks: tracks,
      localIdentity: "me@waddle.test/browser",
      localMicEnabled: true,
      localCameraEnabled: true,
      activeSpeakerIdentities: new Set<string>(),
      volumeRows,
    });

    const self = rows.find((row) => row.isSelf);
    expect(self?.volumeRows).toEqual([]);
    const alice = rows.find((row) => row.label === "alice");
    expect(alice?.volumeRows.map((volume) => volume.source)).toEqual([
      "microphone",
      "screen_share_audio",
    ]);
    const bob = rows.find((row) => row.label === "bob");
    expect(bob?.volumeRows.map((volume) => volume.source)).toEqual(["microphone"]);
  });

  test("includes a track-only attendee not in the roster list, and dedups identities", () => {
    const rows = buildCallRoster({
      // Duplicate (differently-cased) identity must collapse to one row.
      remoteParticipantIdentities: ["alice@waddle.test/web", "ALICE@waddle.test/web"],
      // Dave is present only via a published track — still an attendee.
      remoteTracks: [
        remoteTrack("dave-cam", "dave@waddle.test/phone", "video", "camera"),
      ],
      localIdentity: "me@waddle.test/browser",
      localMicEnabled: true,
      localCameraEnabled: true,
      activeSpeakerIdentities: new Set<string>(),
      volumeRows: [],
    });

    expect(rows.map((row) => row.label)).toEqual(["You", "alice", "dave"]);
    expect(rows.find((row) => row.label === "dave")).toMatchObject({ cameraOn: true });
  });
});
