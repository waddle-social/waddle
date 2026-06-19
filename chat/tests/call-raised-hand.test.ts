import { describe, expect, test } from "bun:test";

import {
  raisedHandPresenceUpdate,
  reduceRaisedHands,
} from "../src/lib/calls/call-raised-hand";

describe("raised-hand reducer", () => {
  test("set raises a participant's hand keyed by identity", () => {
    const next = reduceRaisedHands(
      {},
      {
        kind: "set",
        roomJid: "room@muc.test",
        identityKey: "alice@muc.test/web",
        raised: true,
      },
    );
    expect(next).toEqual({ "room@muc.test": ["alice@muc.test/web"] });
  });

  test("lowering removes the participant and prunes the empty room", () => {
    const raised = { "room@muc.test": ["alice@muc.test/web"] };
    const next = reduceRaisedHands(raised, {
      kind: "set",
      roomJid: "room@muc.test",
      identityKey: "alice@muc.test/web",
      raised: false,
    });
    expect(next).toEqual({});
  });

  test("a second raised hand coexists and the list stays sorted", () => {
    let state = reduceRaisedHands(
      {},
      { kind: "set", roomJid: "r@muc.test", identityKey: "bob@muc.test/x", raised: true },
    );
    state = reduceRaisedHands(state, {
      kind: "set",
      roomJid: "r@muc.test",
      identityKey: "alice@muc.test/y",
      raised: true,
    });
    expect(state["r@muc.test"]).toEqual(["alice@muc.test/y", "bob@muc.test/x"]);
  });

  test("raising an already-raised hand is idempotent", () => {
    const raised = { "r@muc.test": ["a@muc.test/1"] };
    const next = reduceRaisedHands(raised, {
      kind: "set",
      roomJid: "r@muc.test",
      identityKey: "a@muc.test/1",
      raised: true,
    });
    expect(next).toEqual(raised);
  });

  test("clear-room drops only that room", () => {
    const state = { r1: ["a@x/1"], r2: ["b@x/1"] };
    expect(reduceRaisedHands(state, { kind: "clear-room", roomJid: "r1" })).toEqual({
      r2: ["b@x/1"],
    });
  });

  test("does not mutate the input record", () => {
    const input = { "r@muc.test": ["a@x/1"] };
    const snapshot = structuredClone(input);
    reduceRaisedHands(input, {
      kind: "set",
      roomJid: "r@muc.test",
      identityKey: "b@x/2",
      raised: true,
    });
    expect(input).toEqual(snapshot);
  });
});

describe("raisedHandPresenceUpdate", () => {
  test("maps an active raised-hand presence to a set action keyed by real JID", () => {
    expect(
      raisedHandPresenceUpdate({
        from: "room@muc.test/alice",
        muc_jid: "alice@host/web",
        hand_raised: true,
      }),
    ).toEqual({
      kind: "set",
      roomJid: "room@muc.test",
      identityKey: "alice@host/web",
      raised: true,
    });
  });

  test("absent hand_raised lowers the hand", () => {
    expect(
      raisedHandPresenceUpdate({
        from: "room@muc.test/alice",
        muc_jid: "alice@host/web",
      })?.raised,
    ).toBe(false);
  });

  test("unavailable presence lowers the hand even when the flag is set", () => {
    expect(
      raisedHandPresenceUpdate({
        from: "room@muc.test/alice",
        presence_type: "unavailable",
        muc_jid: "alice@host/web",
        hand_raised: true,
      })?.raised,
    ).toBe(false);
  });

  test("returns null when the occupant's real JID is unknown", () => {
    expect(
      raisedHandPresenceUpdate({ from: "room@muc.test/alice", hand_raised: true }),
    ).toBeNull();
  });
});
