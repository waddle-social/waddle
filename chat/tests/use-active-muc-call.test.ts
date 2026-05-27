import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { $callState } from "../src/lib/calls/call-store";
import {
  $mucCallParticipants,
  applyMucCallPresence,
  clearMucCallParticipants,
} from "../src/lib/calls/muc-call-presence";
import { connectionStore } from "../src/lib/connection-store";
import { readActiveMucCall, readRoomHasActiveCall } from "../src/lib/calls/use-active-muc-call";
import {
  clearAllMucCallSessionCacheForTests,
  markMucCallSessionTerminatePending,
} from "../src/lib/calls/muc-call-session-cache";
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
    connectionStore.client = null;
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
    clearAllMucCallSessionCacheForTests();
    setSession(null);
  });

  afterEach(() => {
    $callState.set({ phase: "idle" });
    clearMucCallParticipants();
    clearAllMucCallSessionCacheForTests();
    setSession(null);
  });

  test("returns nulls when the call is idle", () => {
    expect(readActiveMucCall()).toEqual({
      activeRoomJid: null,
      selfInCall: false,
      participantCount: 0,
      media: { audio: true, video: false },
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
      media: { audio: true, video: false },
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
      media: { audio: true, video: true },
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
      media: { audio: true, video: true },
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

describe("readRoomHasActiveCall selector", () => {
  beforeEach(() => {
    $callState.set({ phase: "idle" });
    clearMucCallParticipants();
    clearAllMucCallSessionCacheForTests();
    setSession(null);
  });

  afterEach(() => {
    $callState.set({ phase: "idle" });
    clearMucCallParticipants();
    clearAllMucCallSessionCacheForTests();
    setSession(null);
  });

  test("reports a room call from Muji presence even when local call state is idle", () => {
    setSession("carol");
    $callState.set({ phase: "idle" });
    $mucCallParticipants.set({ [ROOM]: ["alice", "bob"] });

    expect(readRoomHasActiveCall(ROOM)).toEqual({
      hasActiveCall: true,
      selfInCall: false,
      localResourceInCall: false,
      participantCount: 2,
      media: { audio: true, video: false },
    });
  });

  test("distinguishes same-nick presence from this browser resource being in media", () => {
    setSession("alice");
    $callState.set({ phase: "idle" });
    $mucCallParticipants.set({ [ROOM]: ["alice", "bob"] });

    expect(readRoomHasActiveCall(ROOM)).toEqual({
      hasActiveCall: true,
      selfInCall: true,
      localResourceInCall: false,
      participantCount: 2,
      media: { audio: true, video: false },
    });
  });

  test("reports localResourceInCall when this browser owns the active MUC call", () => {
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

    expect(readRoomHasActiveCall(ROOM)).toEqual({
      hasActiveCall: true,
      selfInCall: true,
      localResourceInCall: true,
      participantCount: 2,
      media: { audio: true, video: true },
    });
  });

  test("distinguishes same-resource Muji presence after refresh from another resource", () => {
    setSession("alice");
    connectionStore.client = { fullJid: "alice@waddle.test/web" } as unknown as typeof connectionStore.client;
    $callState.set({ phase: "idle" });
    applyMucCallPresence({
      from: `${ROOM}/alice`,
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: { preparing: false, active: true },
    });

    expect(readRoomHasActiveCall(ROOM)).toEqual({
      hasActiveCall: true,
      selfInCall: true,
      localResourceInCall: false,
      participantCount: 1,
      media: { audio: true, video: false },
    });

    applyMucCallPresence({
      from: `${ROOM}/alice`,
      presence_type: "available",
      muc_jid: "alice@waddle.test/web",
      muji: { preparing: false, active: true },
    });

    expect(readRoomHasActiveCall(ROOM)).toEqual({
      hasActiveCall: true,
      selfInCall: true,
      localResourceInCall: true,
      participantCount: 1,
      media: { audio: true, video: false },
    });
  });

  test("treats ownerless self Muji presence as a retained local resource after refresh", () => {
    setSession("alice");
    connectionStore.client = { fullJid: "alice@waddle.test/web" } as unknown as typeof connectionStore.client;
    $callState.set({ phase: "idle" });
    applyMucCallPresence({
      from: `${ROOM}/alice`,
      presence_type: "available",
      muji: { preparing: false, active: true },
    });

    expect(readRoomHasActiveCall(ROOM)).toEqual({
      hasActiveCall: true,
      selfInCall: true,
      localResourceInCall: true,
      participantCount: 1,
      media: { audio: true, video: false },
    });
  });

  test("preserves resourcepart case when matching Muji ownership to this browser", () => {
    setSession("alice");
    connectionStore.client = { fullJid: "alice@waddle.test/Web" } as unknown as typeof connectionStore.client;
    $callState.set({ phase: "idle" });
    applyMucCallPresence({
      from: `${ROOM}/alice`,
      presence_type: "available",
      muc_jid: "alice@waddle.test/web",
      muji: { preparing: false, active: true },
    });

    expect(readRoomHasActiveCall(ROOM).localResourceInCall).toBe(false);

    applyMucCallPresence({
      from: `${ROOM}/alice`,
      presence_type: "available",
      muc_jid: "alice@waddle.test/Web",
      muji: { preparing: false, active: true },
    });

    expect(readRoomHasActiveCall(ROOM).localResourceInCall).toBe(true);
  });

  test("normalizes resource-suffixed room JIDs", () => {
    setSession("alice");
    $mucCallParticipants.set({ [ROOM]: ["bob"] });

    expect(readRoomHasActiveCall(`${ROOM}/alice`)).toEqual({
      hasActiveCall: true,
      selfInCall: false,
      localResourceInCall: false,
      participantCount: 1,
      media: { audio: true, video: false },
    });
  });

  test("treats missing or empty rooms as no active call", () => {
    setSession("alice");
    $mucCallParticipants.set({ [ROOM]: ["alice"] });

    expect(readRoomHasActiveCall("")).toEqual({
      hasActiveCall: false,
      selfInCall: false,
      localResourceInCall: false,
      participantCount: 0,
      media: { audio: true, video: false },
    });
    expect(readRoomHasActiveCall("other@muc.waddle.test")).toEqual({
      hasActiveCall: false,
      selfInCall: false,
      localResourceInCall: false,
      participantCount: 0,
      media: { audio: true, video: false },
    });
  });

  test("reports video media from Muji content presence", () => {
    setSession("alice");
    applyMucCallPresence({
      from: `${ROOM}/alice`,
      presence_type: "available",
      muc_jid: "alice@waddle.test/web",
      muji: { preparing: false, active: true, audio: true, video: true },
    });

    expect(readRoomHasActiveCall(ROOM).media).toEqual({ audio: true, video: true });
  });

  test("keeps a cached terminate failure retryable after local Muji presence is cleared", () => {
    setSession("alice");
    connectionStore.client = { fullJid: "alice@waddle.test/web" } as unknown as typeof connectionStore.client;
    markMucCallSessionTerminatePending({
      roomJid: ROOM,
      sid: "muc-retry-live",
      selfFullJid: "alice@waddle.test/web",
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    expect(readRoomHasActiveCall(ROOM)).toEqual({
      hasActiveCall: true,
      selfInCall: true,
      localResourceInCall: true,
      participantCount: 1,
      media: { audio: true, video: false },
    });
  });
});
