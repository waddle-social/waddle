import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { $callState } from "../src/lib/calls/call-store";
import {
  $mucCallParticipants,
  clearMucCallParticipants,
} from "../src/lib/calls/muc-call-presence";
import { connectionStore } from "../src/lib/connection-store";
import { readActiveMucCall } from "../src/lib/calls/use-active-muc-call";
import type { CallMedia, LiveKitJoin } from "../src/lib/calls/types";
import type { WaddleSession } from "../src/lib/server-auth";

/**
 * Imperative readActiveMucCall is the testable surface — the
 * composable wraps it with Vue refs, but the Vue setup context is
 * awkward to spin up in Bun's test env. Both forms share the same
 * read logic, so exercising the imperative form covers the selector.
 */

const ROOM = "design@muc.waddle.test";
const audioVideo: CallMedia = { audio: true, video: true };
const join: LiveKitJoin = {
  url: "wss://livekit.test",
  room: ROOM,
  identity: "alice@waddle.test/web",
  token: "tok",
};

function setSession(username: string | null): void {
  if (username === null) {
    connectionStore.session = null;
    return;
  }
  connectionStore.session = {
    username,
    jid: `${username}@waddle.test`,
  } as unknown as WaddleSession;
}

describe("readActiveMucCall selector", () => {
  beforeEach(() => {
    $callState.set({ phase: "idle" });
    clearMucCallParticipants();
    setSession(null);
  });

  afterEach(() => {
    $callState.set({ phase: "idle" });
    clearMucCallParticipants();
    setSession(null);
  });

  test("returns nulls when the call is idle", () => {
    expect(readActiveMucCall()).toEqual({
      activeRoomJid: null,
      selfInCall: false,
      participantCount: 0,
    });
  });

  test("returns nulls for an active DM call (only MUC calls qualify)", () => {
    setSession("alice");
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@waddle.test/web",
      sid: "c1",
      media: audioVideo,
      join,
    });
    $mucCallParticipants.set({ [ROOM]: ["alice"] });
    expect(readActiveMucCall()).toEqual({
      activeRoomJid: null,
      selfInCall: false,
      participantCount: 0,
    });
  });

  test("returns the room and selfInCall=true when the local nick is in the call", () => {
    setSession("alice");
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: ROOM,
      sid: "c1",
      media: audioVideo,
      join,
      selfNick: "alice",
    });
    $mucCallParticipants.set({ [ROOM]: ["alice", "bob"] });
    expect(readActiveMucCall()).toEqual({
      activeRoomJid: ROOM,
      selfInCall: true,
      participantCount: 2,
    });
  });

  test("returns selfInCall=false when the local nick is not yet in the participants list", () => {
    setSession("carol");
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: ROOM,
      sid: "c1",
      media: audioVideo,
      join,
      selfNick: "carol",
    });
    $mucCallParticipants.set({ [ROOM]: ["alice", "bob"] });
    expect(readActiveMucCall()).toEqual({
      activeRoomJid: ROOM,
      selfInCall: false,
      participantCount: 2,
    });
  });

  test("normalizes the call's peer JID so a resource-suffixed peer still matches the room", () => {
    setSession("alice");
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: `${ROOM}/alice`,
      sid: "c1",
      media: audioVideo,
      join,
      selfNick: "alice",
    });
    $mucCallParticipants.set({ [ROOM]: ["alice"] });
    expect(readActiveMucCall().activeRoomJid).toBe(ROOM);
  });

  test("treats no session as not-in-call regardless of participant entries", () => {
    setSession(null);
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: ROOM,
      sid: "c1",
      media: audioVideo,
      join,
      selfNick: "alice",
    });
    $mucCallParticipants.set({ [ROOM]: ["alice"] });
    expect(readActiveMucCall().selfInCall).toBe(false);
  });
});
