import { describe, expect, test } from "bun:test";
import { remoteParticipantIdentitiesForCall } from "../src/lib/calls/call-remote-identities";
import type { CallState } from "../src/lib/calls/types";
import type { RemoteMediaTrack } from "../src/lib/calls/engine";

const fakeTrack = {} as never;

function remoteTrack(participantIdentity: string): RemoteMediaTrack {
  return {
    participantIdentity,
    publicationSid: `${participantIdentity}-pub`,
    kind: "audio",
    source: "microphone",
    track: fakeTrack,
  };
}

const mucState: CallState = {
  phase: "active",
  peer: "room@muc.waddle.test",
  sid: "c1",
  media: { audio: true, video: false },
  join: { url: "wss://livekit.test", room: "room@muc.waddle.test", identity: "me", token: "jwt" },
  kind: "muc",
  selfNick: "me",
  selfFullJid: "me@waddle.test/browser",
};

const dmState: CallState = {
  phase: "active",
  peer: "bob@waddle.test/desktop",
  sid: "c1",
  media: { audio: true, video: false },
  join: { url: "wss://livekit.test", room: "bob@waddle.test::c1", identity: "me", token: "jwt" },
  kind: "dm",
};

describe("remoteParticipantIdentitiesForCall", () => {
  test("a MUC call resolves to its live Muji/LiveKit roster", () => {
    expect(
      remoteParticipantIdentitiesForCall({
        state: mucState,
        liveParticipantsByRoom: { "room@muc.waddle.test": ["alice@waddle.test/web"] },
        normalizedRoomJid: "room@muc.waddle.test",
        remoteTracks: [],
      }),
    ).toEqual(["alice@waddle.test/web"]);
  });

  test("a DM call resolves to the peer plus any identities we have a track for", () => {
    expect(
      remoteParticipantIdentitiesForCall({
        state: dmState,
        liveParticipantsByRoom: {},
        normalizedRoomJid: "",
        remoteTracks: [remoteTrack("bob@waddle.test/desktop")],
      }),
    ).toEqual(["bob@waddle.test/desktop"]);
  });
});
