import { describe, expect, test } from "bun:test";

import { mutePresenceUpdate, reduceMutedParticipants } from "../src/lib/calls/call-mute";

describe("muted-participants reducer", () => {
  test("set marks a participant muted keyed by identity", () => {
    const next = reduceMutedParticipants(
      {},
      {
        kind: "set",
        roomJid: "room@muc.test",
        identityKey: "alice@muc.test/web",
        muted: true,
      },
    );
    expect(next).toEqual({ "room@muc.test": ["alice@muc.test/web"] });
  });

  test("unmuting removes the participant and prunes the empty room", () => {
    const muted = { "room@muc.test": ["alice@muc.test/web"] };
    const next = reduceMutedParticipants(muted, {
      kind: "set",
      roomJid: "room@muc.test",
      identityKey: "alice@muc.test/web",
      muted: false,
    });
    expect(next).toEqual({});
  });

  test("a second muted participant coexists and the list stays sorted", () => {
    let state = reduceMutedParticipants(
      {},
      { kind: "set", roomJid: "r@muc.test", identityKey: "bob@muc.test/x", muted: true },
    );
    state = reduceMutedParticipants(state, {
      kind: "set",
      roomJid: "r@muc.test",
      identityKey: "alice@muc.test/y",
      muted: true,
    });
    expect(state["r@muc.test"]).toEqual(["alice@muc.test/y", "bob@muc.test/x"]);
  });

  test("muting an already-muted participant is idempotent", () => {
    const muted = { "r@muc.test": ["a@muc.test/1"] };
    const next = reduceMutedParticipants(muted, {
      kind: "set",
      roomJid: "r@muc.test",
      identityKey: "a@muc.test/1",
      muted: true,
    });
    expect(next).toEqual(muted);
  });

  test("clear-room drops only that room", () => {
    const state = { r1: ["a@x/1"], r2: ["b@x/1"] };
    expect(reduceMutedParticipants(state, { kind: "clear-room", roomJid: "r1" })).toEqual({
      r2: ["b@x/1"],
    });
  });

  test("returns the same reference for no-op updates (no spurious rerenders)", () => {
    const state = { "r@muc.test": ["a@x/1"] };
    // Muting an already-muted participant.
    expect(
      reduceMutedParticipants(state, {
        kind: "set",
        roomJid: "r@muc.test",
        identityKey: "a@x/1",
        muted: true,
      }),
    ).toBe(state);
    // Unmuting a participant that was never muted.
    expect(
      reduceMutedParticipants(state, {
        kind: "set",
        roomJid: "r@muc.test",
        identityKey: "nobody@x/1",
        muted: false,
      }),
    ).toBe(state);
    // clear-room for a room with no mutes.
    expect(
      reduceMutedParticipants(state, { kind: "clear-room", roomJid: "other@muc.test" }),
    ).toBe(state);
    // clear-all on an already-empty record.
    const empty = {};
    expect(reduceMutedParticipants(empty, { kind: "clear-all" })).toBe(empty);
  });

  test("does not mutate the input record", () => {
    const input = { "r@muc.test": ["a@x/1"] };
    const snapshot = structuredClone(input);
    reduceMutedParticipants(input, {
      kind: "set",
      roomJid: "r@muc.test",
      identityKey: "b@x/2",
      muted: true,
    });
    expect(input).toEqual(snapshot);
  });
});

describe("mutePresenceUpdate", () => {
  test("maps a muted presence to a set action keyed by real JID", () => {
    expect(
      mutePresenceUpdate({
        from: "room@muc.test/alice",
        muc_jid: "alice@host/web",
        muted: true,
      }),
    ).toEqual({
      kind: "set",
      roomJid: "room@muc.test",
      identityKey: "alice@host/web",
      muted: true,
    });
  });

  test("absent muted flag unmutes", () => {
    expect(
      mutePresenceUpdate({
        from: "room@muc.test/alice",
        muc_jid: "alice@host/web",
      })?.muted,
    ).toBe(false);
  });

  test("unavailable presence unmutes even when the flag is set", () => {
    expect(
      mutePresenceUpdate({
        from: "room@muc.test/alice",
        presence_type: "unavailable",
        muc_jid: "alice@host/web",
        muted: true,
      })?.muted,
    ).toBe(false);
  });

  test("returns null when the occupant's real JID is unknown", () => {
    expect(mutePresenceUpdate({ from: "room@muc.test/alice", muted: true })).toBeNull();
  });
});
