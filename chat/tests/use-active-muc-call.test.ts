import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { $callState } from "../src/lib/calls/call-store";
import {
  $mucCallParticipants,
  clearMucCallParticipants,
} from "../src/lib/calls/muc-call-presence";
import { connectionStore } from "../src/lib/connection-store";
import { readActiveMucCall, readRoomHasActiveCall } from "../src/lib/calls/use-active-muc-call";
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

describe("readRoomHasActiveCall selector — populated from Muji presence alone", () => {
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

  test("post-refresh discovery: room has a call even though local `$callState` is idle", () => {
    // This is the exact scenario the user reported: a page refresh
    // clears `$callState` but the rejoined client receives roster
    // replay with XEP-0272 Muji extensions from existing occupants.
    // The pill MUST light up on the strength of that store alone.
    setSession("carol");
    $callState.set({ phase: "idle" });
    $mucCallParticipants.set({ [ROOM]: ["alice", "bob"] });

    expect(readRoomHasActiveCall(ROOM)).toEqual({
      hasActiveCall: true,
      selfInCall: false,
      participantCount: 2,
    });
  });

  test("self is in call when the local nick appears in the participants list", () => {
    setSession("alice");
    $mucCallParticipants.set({ [ROOM]: ["alice", "bob"] });

    expect(readRoomHasActiveCall(ROOM)).toEqual({
      hasActiveCall: true,
      selfInCall: true,
      participantCount: 2,
    });
  });

  test("no call when the room is absent from the store", () => {
    setSession("alice");
    $mucCallParticipants.set({});

    expect(readRoomHasActiveCall(ROOM)).toEqual({
      hasActiveCall: false,
      selfInCall: false,
      participantCount: 0,
    });
  });

  test("no call when the room's participants list is empty (XEP-0272 §Leaving cleared it)", () => {
    setSession("alice");
    $mucCallParticipants.set({ [ROOM]: [] });

    expect(readRoomHasActiveCall(ROOM)).toEqual({
      hasActiveCall: false,
      selfInCall: false,
      participantCount: 0,
    });
  });

  test("resource-suffixed roomJid still matches the canonical bare key", () => {
    setSession("alice");
    $mucCallParticipants.set({ [ROOM]: ["bob"] });

    expect(readRoomHasActiveCall(`${ROOM}/alice`).hasActiveCall).toBe(true);
  });

  test("empty roomJid is a no-op", () => {
    setSession("alice");
    $mucCallParticipants.set({ [ROOM]: ["alice"] });

    expect(readRoomHasActiveCall("")).toEqual({
      hasActiveCall: false,
      selfInCall: false,
      participantCount: 0,
    });
  });
});
