import { afterEach, describe, expect, test } from "bun:test";
import { computed, effectScope } from "vue";
import { buildCallRoster } from "../src/lib/calls/call-roster";
import { useCallRoster } from "../src/lib/calls/use-call-roster";
import { buildCallVolumeMixerRows } from "../src/lib/calls/call-volume-mixer";
import { useCallEngine } from "../src/lib/calls/use-call-engine";
import { $callState, clearCallState } from "../src/lib/calls/call-store";
import {
  clearAllLiveCallParticipants,
  setLiveCallParticipants,
} from "../src/lib/calls/muc-call-live-participants";
import type { CallVolumeMixerRow } from "../src/lib/calls/call-volume-mixer";
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

  test("flags raised hands by identity, including self", () => {
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
      raisedHandKeys: new Set<string>(["alice@waddle.test/web"]),
      selfRaisedHand: true,
    });

    const byLabel = Object.fromEntries(rows.map((row) => [row.label, row.raisedHand]));
    expect(byLabel).toEqual({ You: true, alice: true, bob: false });
  });

  test("defaults raisedHand to false when no raised-hand input is given", () => {
    const rows = buildCallRoster({
      remoteParticipantIdentities: ["alice@waddle.test/web"],
      remoteTracks: [],
      localIdentity: "me@waddle.test/browser",
      localMicEnabled: true,
      localCameraEnabled: false,
      activeSpeakerIdentities: new Set<string>(),
      volumeRows: [],
    });
    expect(rows.every((row) => row.raisedHand === false)).toBe(true);
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

  test("presence-advertised mute overrides a live audio track for remote attendees (#1030)", () => {
    const rows = buildCallRoster({
      remoteParticipantIdentities: [
        "alice@waddle.test/web",
        "bob@waddle.test/desktop",
      ],
      // Both keep a published mic track — LiveKit's default leaves the track
      // live while muted, so track presence alone cannot tell us mute state.
      remoteTracks: [
        remoteTrack("alice-mic", "alice@waddle.test/web", "audio", "microphone"),
        remoteTrack("bob-mic", "bob@waddle.test/desktop", "audio", "microphone"),
      ],
      localIdentity: "me@waddle.test/browser",
      localMicEnabled: true,
      localCameraEnabled: false,
      activeSpeakerIdentities: new Set<string>(),
      volumeRows: [],
      mutedKeys: new Set<string>(["alice@waddle.test/web"]),
    });

    const byLabel = Object.fromEntries(rows.map((row) => [row.label, row.micOn]));
    // Authoritative XMPP presence mute wins over the live track.
    expect(byLabel.alice).toBe(false);
    expect(byLabel.bob).toBe(true);
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

describe("useCallRoster controller", () => {
  const callJoin = {
    url: "wss://livekit.test",
    room: "room@muc.waddle.test",
    identity: "me@waddle.test/browser",
    token: "jwt",
  };

  afterEach(() => {
    clearCallState();
    clearAllLiveCallParticipants();
  });

  function activateMucCall(sid: string): void {
    $callState.set({
      phase: "active",
      peer: "room@muc.waddle.test",
      sid,
      media: { audio: true, video: false },
      join: { ...callJoin },
      kind: "muc",
      selfNick: "me",
      selfFullJid: "me@waddle.test/browser",
    });
  }

  test("projects self plus live MUC participants and delegates setVolume to the engine", () => {
    const captured = captureEngineVolumeCalls();
    const scope = effectScope();
    try {
      activateMucCall("c1");
      setLiveCallParticipants("room@muc.waddle.test", ["alice@waddle.test/web"]);

      let controller!: ReturnType<typeof useCallRoster>;
      scope.run(() => {
        controller = useCallRoster(computed(() => "room@muc.waddle.test"));
      });

      expect(controller.rows.value.map((row) => row.label)).toEqual(["You", "alice"]);

      const alice = controller.rows.value.find((row) => row.label === "alice");
      const micRow = alice?.volumeRows[0];
      expect(micRow?.source).toBe("microphone");
      controller.setVolume(micRow as CallVolumeMixerRow, 0.5);
      expect(captured.calls).toContainEqual({
        participantIdentity: "alice@waddle.test/web",
        source: "microphone",
        volume: 0.5,
      });
    } finally {
      scope.stop();
      captured.restore();
    }
  });

  test("excludes the local identity from the remote rows once the live list seeds self", () => {
    const scope = effectScope();
    try {
      activateMucCall("c1");
      // The `connected` handler seeds the live list with our own join
      // identity alongside peers. The roster must dedupe self against it —
      // a stale (non-reactive) local identity would list the local user
      // twice. join.identity here is "me@waddle.test/browser".
      setLiveCallParticipants("room@muc.waddle.test", [
        "me@waddle.test/browser",
        "alice@waddle.test/web",
      ]);

      let controller!: ReturnType<typeof useCallRoster>;
      scope.run(() => {
        controller = useCallRoster(computed(() => "room@muc.waddle.test"));
      });

      expect(controller.rows.value.map((row) => row.label)).toEqual(["You", "alice"]);
      expect(controller.rows.value.filter((row) => row.isSelf)).toHaveLength(1);
    } finally {
      scope.stop();
    }
  });
});

function captureEngineVolumeCalls() {
  const { engine } = useCallEngine();
  const calls: Array<{ participantIdentity: string; source: string; volume: number }> = [];
  const target = engine as unknown as {
    setParticipantAudioVolume: (input: {
      participantIdentity: string;
      source: string;
      volume: number;
    }) => void;
  };
  const original = target.setParticipantAudioVolume;
  target.setParticipantAudioVolume = (input) => {
    calls.push(input);
  };
  return {
    calls,
    restore() {
      target.setParticipantAudioVolume = original;
    },
  };
}
