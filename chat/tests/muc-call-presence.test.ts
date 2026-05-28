import { afterEach, describe, expect, test } from "bun:test";
import {
  $mucCallParticipants,
  applyMucCallPresence,
  awaitNoOtherPreparing,
  awaitPreparingEcho,
  clearMucCallParticipant,
  clearMucCallParticipants,
  mucCallParticipantCounts,
  mucCallParticipantCount,
} from "../src/lib/calls/muc-call-presence";

afterEach(() => {
  clearMucCallParticipants();
});

// XEP-0272 Muji presence — the namespace + element shape that the
// chat-side store consumes. The previous custom `urn:waddle:muc-call:0`
// extension has been retired; these tests exercise the active/preparing
// boolean pair that the WASM parser surfaces in `WasmMujiPresence`.
const activeMuji = { preparing: false, active: true } as const;
const preparingMuji = { preparing: true, active: false } as const;
const activePreparingMuji = { preparing: true, active: true } as const;

describe("applyMucCallPresence", () => {
  test("available presence with active Muji registers the nick", () => {
    applyMucCallPresence({
      from: "Room@MUC.Test/alice",
      presence_type: "available",
      muji: activeMuji,
    });
    expect($mucCallParticipants.get()).toEqual({
      "room@muc.test": ["alice"],
    });
    expect(mucCallParticipantCount("room@muc.test")).toBe(1);
    expect(mucCallParticipantCount("ROOM@MUC.TEST/resource")).toBe(1);
  });

  test("multiple occupants accumulate per room", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activeMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test/bob",
      presence_type: "available",
      muji: activeMuji,
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toEqual([
      "alice",
      "bob",
    ]);
  });

  test("preparing-only Muji does NOT count as active (XEP-0272 §Joining two-phase flow)", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: preparingMuji,
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toBeUndefined();
  });

  test("preparing-only Muji preserves an existing active nick for same-nick sibling joins", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activeMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: preparingMuji,
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toEqual(["alice"]);
  });

  test("plain same-nick sibling clear preserves the exact active owner", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/desktop",
      muji: activeMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toEqual(["alice"]);

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/desktop",
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toBeUndefined();
  });

  test("clearing one active same-nick owner preserves another active owner", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/desktop",
      muji: activeMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: activeMuji,
    });

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/desktop",
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toEqual(["alice"]);

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toBeUndefined();
  });

  test("local exact clear preserves active same-nick siblings", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/desktop",
      muji: activeMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: activeMuji,
    });

    clearMucCallParticipant("room@muc.test", "alice", "alice@waddle.test/desktop");
    expect($mucCallParticipants.get()["room@muc.test"]).toEqual(["alice"]);

    clearMucCallParticipant("room@muc.test", "alice", "alice@waddle.test/mobile");
    expect($mucCallParticipants.get()["room@muc.test"]).toBeUndefined();
  });

  test("departed active same-nick resource clears before preparing sibling replay", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/desktop",
      muji: activeMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: preparingMuji,
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toEqual(["alice"]);

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/desktop",
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: preparingMuji,
    });

    expect($mucCallParticipants.get()["room@muc.test"]).toBeUndefined();
  });

  test("active+preparing Muji counts as active while preserving the setup signal", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activePreparingMuji,
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toEqual(["alice"]);
  });

  test("available presence WITHOUT the Muji extension removes a previously-registered nick (XEP-0272 §Leaving)", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activeMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toBeUndefined();
  });

  test("unavailable presence removes the nick (occupant left the room entirely)", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activeMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "unavailable",
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toBeUndefined();
  });

  test("rooms are tracked independently", () => {
    applyMucCallPresence({
      from: "room-a@muc.test/alice",
      presence_type: "available",
      muji: activeMuji,
    });
    applyMucCallPresence({
      from: "room-b@muc.test/carol",
      presence_type: "available",
      muji: activeMuji,
    });
    expect($mucCallParticipants.get()).toEqual({
      "room-a@muc.test": ["alice"],
      "room-b@muc.test": ["carol"],
    });
  });

  test("re-adding an already-present nick is idempotent", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activeMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activeMuji,
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toEqual(["alice"]);
  });

  test("removing a never-added nick is a no-op", () => {
    applyMucCallPresence({
      from: "room@muc.test/ghost",
      presence_type: "unavailable",
    });
    expect($mucCallParticipants.get()).toEqual({});
  });

  test("ignores presences missing a `from` or nick", () => {
    applyMucCallPresence({
      presence_type: "available",
      muji: activeMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test",
      presence_type: "available",
      muji: activeMuji,
    });
    expect($mucCallParticipants.get()).toEqual({});
  });

  test("clearMucCallParticipants drops every tracked room", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activeMuji,
    });
    clearMucCallParticipants();
    expect($mucCallParticipants.get()).toEqual({});
  });

  test("clearMucCallParticipants rejects pending preparing echo waiters", async () => {
    const result = awaitPreparingEcho("room@muc.test", "alice", 5000)
      .then(() => "resolved" as const)
      .catch((err) => err);
    clearMucCallParticipants();
    const err = await result;
    expect(err).toBeInstanceOf(Error);
    expect(String(err.message)).toContain("cancelled");
  });

  test("mucCallParticipantCounts normalizes room keys for sidebar/header indicators", () => {
    expect(mucCallParticipantCounts({
      "Room@MUC.Test/web": ["alice"],
      "room@muc.test": ["bob"],
      "other@muc.test": ["carol"],
    })).toEqual({
      "room@muc.test": 2,
      "other@muc.test": 1,
    });
  });

  test("registers the local user's own nick when their sibling resource starts a call (regression: cross-instance indicator)", () => {
    // Scenario: alice is signed in on two browser instances (web +
    // mobile). Both have joined `room@muc.test` with the same nick
    // "alice" — XEP-0045 §7.2 same-bare multi-session join. Web
    // starts a call; the server reflects the Muji presence to BOTH
    // sessions per XEP-0045 §7.1. Mobile's wasm bridge surfaces the
    // reflection here.
    //
    // The store MUST register "alice" as an in-call nick on mobile
    // too — otherwise mobile's sidebar chip never lights up and the
    // user has to manually re-discover the call. This was the
    // user-visible symptom of the server-side delivery bug
    // (`muc_update.rs` routing self-bare recipients onto the
    // sender's WebSocket via `responses` instead of the sibling's
    // via the connection registry).
    //
    // Pin the contract: a presence whose `from` reflects our own
    // identity is NOT filtered out. The store has no concept of
    // "self" — and intentionally so. A future refactor that adds an
    // implicit self-filter would silently re-break the multi-
    // instance indicator without any other test catching it.
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activeMuji,
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toEqual(["alice"]);
    expect(mucCallParticipantCount("room@muc.test")).toBe(1);

    // The matching XEP-0272 §Leaving reflection (empty `<muji/>`
    // gets stripped by the server, so the chat side sees an
    // available presence with no muji field) must clear the entry
    // even though it reflects our own identity.
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toBeUndefined();
  });
});

describe("awaitPreparingEcho (XEP-0272 §Joining MUST)", () => {
  test("resolves when a matching preparing-only presence echo arrives", async () => {
    // The pre-call flow registers an echo waiter then fires the
    // preparing presence; the MUC echoes it back; the waiter
    // resolves so beginMucCall can proceed.
    const wait = awaitPreparingEcho("room@muc.test", "alice", 5000);
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: preparingMuji,
    });
    await wait;
    // No assertion needed — the await itself is the test. If the
    // echo never fires the listener, the test times out.
  });

  test("does NOT resolve on a different nick's preparing echo", async () => {
    // Two participants prepare at the same time; alice's waiter
    // must NOT resolve on bob's echo.
    const aliceWait = awaitPreparingEcho("room@muc.test", "alice", 200);
    let aliceResolved = false;
    aliceWait.then(() => {
      aliceResolved = true;
    });
    applyMucCallPresence({
      from: "room@muc.test/bob",
      presence_type: "available",
      muji: preparingMuji,
    });
    // Give the microtask queue a chance to run before checking.
    await new Promise((r) => setTimeout(r, 10));
    expect(aliceResolved).toBe(false);
    // Now alice's echo arrives:
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: preparingMuji,
    });
    await aliceWait;
    expect(aliceResolved).toBe(true);
  });

  test("does NOT resolve on a same-nick sibling resource's preparing echo", async () => {
    const aliceWait = awaitPreparingEcho(
      "room@muc.test",
      "alice",
      200,
      "alice@waddle.test/web",
    );
    let aliceResolved = false;
    void aliceWait.then(() => {
      aliceResolved = true;
    }).catch(() => undefined);

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: preparingMuji,
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(aliceResolved).toBe(false);

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/web",
      muji: preparingMuji,
    });
    await aliceWait;
    expect(aliceResolved).toBe(true);
  });

  test("does NOT resolve on a content-only presence", async () => {
    // A content-only presence is NOT a preparing echo; the waiter
    // must continue to block until the MUC echoes preparing.
    const wait = awaitPreparingEcho("room@muc.test", "alice", 200);
    let resolved = false;
    void wait.then(() => {
      resolved = true;
    }).catch(() => undefined);
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activeMuji, // active, not preparing
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(resolved).toBe(false);
    await expect(wait).rejects.toThrow("Timed out waiting for Muji preparing presence echo");
  });

  test("resolves on active+preparing presence because preparing remains the setup signal", async () => {
    const wait = awaitPreparingEcho(
      "room@muc.test",
      "alice",
      200,
      "alice@waddle.test/web",
    );
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/web",
      muji: activePreparingMuji,
    });
    await wait;
  });

  test("rejects on timeout when no echo arrives", async () => {
    // Missing preparing echo is fatal for strict XEP-0272 setup:
    // beginMucCall must roll back instead of proceeding with room
    // state the MUC never reflected.
    await expect(awaitPreparingEcho("room@muc.test/nobody", "alice", 50))
      .rejects.toThrow("Timed out waiting for Muji preparing presence echo");
  });
});

describe("awaitNoOtherPreparing", () => {
  test("same-nick sibling resources still count as other preparing participants", async () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: preparingMuji,
    });

    const wait = awaitNoOtherPreparing(
      "room@muc.test",
      "alice",
      200,
      "alice@waddle.test/web",
    );
    let resolved = false;
    void wait.then(() => {
      resolved = true;
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(resolved).toBe(false);

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: activeMuji,
    });

    await wait;
    expect(resolved).toBe(true);
  });

  test("active+preparing same-nick sibling remains a blocker until preparing clears", async () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: activePreparingMuji,
    });

    const wait = awaitNoOtherPreparing(
      "room@muc.test",
      "alice",
      200,
      "alice@waddle.test/web",
    );
    let resolved = false;
    void wait.then(() => {
      resolved = true;
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(resolved).toBe(false);
    expect($mucCallParticipants.get()["room@muc.test"]).toEqual(["alice"]);

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: activeMuji,
    });

    await wait;
    expect(resolved).toBe(true);
  });

  test("plain same-nick sibling presence does not clear active+preparing owner", async () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: activePreparingMuji,
    });

    const wait = awaitNoOtherPreparing(
      "room@muc.test",
      "carol",
      200,
      "carol@waddle.test/web",
    );
    let resolved = false;
    void wait.then(() => {
      resolved = true;
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(resolved).toBe(false);

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/desktop",
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(resolved).toBe(false);

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: activeMuji,
    });

    await wait;
    expect(resolved).toBe(true);
  });

  test("active+preparing exact owner leaves sibling preparing blockers intact", async () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: preparingMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/tablet",
      muji: preparingMuji,
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: activePreparingMuji,
    });

    const wait = awaitNoOtherPreparing(
      "room@muc.test",
      "carol",
      200,
      "carol@waddle.test/web",
    );
    let resolved = false;
    void wait.then(() => {
      resolved = true;
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(resolved).toBe(false);

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/tablet",
      muji: activeMuji,
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(resolved).toBe(false);

    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: activeMuji,
    });

    await wait;
    expect(resolved).toBe(true);
  });
});
