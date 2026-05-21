import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  $activeCallStartedAt,
  $callState,
} from "../src/lib/calls/call-store";
import {
  $persistedCallStub,
  __resetPersistedCallStubForTests,
} from "../src/lib/calls/call-persistence";
import type { CallMedia, LiveKitJoin } from "../src/lib/calls/types";

/**
 * `$activeCallStartedAt` and the `$persistedCallStub` share the
 * same module-load `$callState.listen` subscription in
 * `call-store.ts`. Both are derived state, so exercising them
 * together keeps the contract honest: the listener fires
 * synchronously after `$callState.set`, and the two atoms move in
 * lockstep with phase transitions.
 */

const ROOM = "design@muc.waddle.test";
const audioVideo: CallMedia = { audio: true, video: true };
const join: LiveKitJoin = {
  url: "wss://livekit.test",
  room: ROOM,
  identity: "alice@waddle.test/web",
  token: "tok",
};

function reset(): void {
  $callState.set({ phase: "idle" });
  __resetPersistedCallStubForTests();
}

describe("$activeCallStartedAt", () => {
  beforeEach(reset);
  afterEach(reset);

  test("is null when idle", () => {
    expect($activeCallStartedAt.get()).toBeNull();
  });

  test("captures Date.now() on transition into active", () => {
    const before = Date.now();
    $callState.set({
      phase: "active",
      peer: ROOM,
      sid: "c1",
      media: audioVideo,
      join,
      kind: "muc",
    });
    const captured = $activeCallStartedAt.get();
    expect(captured).not.toBeNull();
    expect(captured!).toBeGreaterThanOrEqual(before);
    expect(captured!).toBeLessThanOrEqual(Date.now());
  });

  test("does not move on no-op updates within the same sid", () => {
    $callState.set({
      phase: "active",
      peer: ROOM,
      sid: "c1",
      media: audioVideo,
      join,
      kind: "muc",
    });
    const first = $activeCallStartedAt.get();
    expect(first).not.toBeNull();

    $callState.set({
      phase: "active",
      peer: ROOM,
      sid: "c1",
      media: audioVideo,
      join,
      kind: "muc",
      selfNick: "alice",
    });
    expect($activeCallStartedAt.get()).toBe(first);
  });

  test("resets on a new sid", () => {
    $callState.set({
      phase: "active",
      peer: ROOM,
      sid: "c1",
      media: audioVideo,
      join,
      kind: "muc",
    });
    expect($activeCallStartedAt.get()).not.toBeNull();

    // Drop back through idle so the listener observes the
    // "left active" transition and clears its tracking.
    $callState.set({ phase: "idle" });
    expect($activeCallStartedAt.get()).toBeNull();

    // New sid → the listener treats this as a fresh call and
    // re-stamps. Two consecutive Date.now() reads can land on the
    // same millisecond, so the contract is "non-null again",
    // not "strictly greater than" or "different from prior".
    $callState.set({
      phase: "active",
      peer: ROOM,
      sid: "c2",
      media: audioVideo,
      join,
      kind: "muc",
    });
    expect($activeCallStartedAt.get()).not.toBeNull();
  });

  test("clears when phase leaves active", () => {
    $callState.set({
      phase: "active",
      peer: ROOM,
      sid: "c1",
      media: audioVideo,
      join,
      kind: "muc",
    });
    expect($activeCallStartedAt.get()).not.toBeNull();

    $callState.set({ phase: "ended", sid: "c1", reason: "success" });
    expect($activeCallStartedAt.get()).toBeNull();
  });
});

describe("$persistedCallStub", () => {
  beforeEach(reset);
  afterEach(reset);

  test("is null at rest", () => {
    expect($persistedCallStub.get()).toBeNull();
  });

  test("captures the room JID on active MUC call", () => {
    $callState.set({
      phase: "active",
      peer: ROOM,
      sid: "c1",
      media: audioVideo,
      join,
      kind: "muc",
    });
    const stub = $persistedCallStub.get();
    expect(stub).not.toBeNull();
    expect(stub!.roomJid).toBe(ROOM);
    expect(stub!.joinedAt).toBeGreaterThan(0);
  });

  test("does NOT persist for active DM calls (rejoin shape differs)", () => {
    $callState.set({
      phase: "active",
      peer: "carol@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
    });
    expect($persistedCallStub.get()).toBeNull();
  });

  test("clears when the call ends", () => {
    $callState.set({
      phase: "active",
      peer: ROOM,
      sid: "c1",
      media: audioVideo,
      join,
      kind: "muc",
    });
    expect($persistedCallStub.get()).not.toBeNull();

    $callState.set({ phase: "ended", sid: "c1", reason: "success" });
    expect($persistedCallStub.get()).toBeNull();
  });
});
