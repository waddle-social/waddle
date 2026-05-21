import { afterEach, describe, expect, test } from "bun:test";
import {
  $mucCallParticipants,
  applyMucCallPresence,
  awaitPreparingEcho,
  clearMucCallParticipants,
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

describe("applyMucCallPresence", () => {
  test("available presence with active Muji registers the nick", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activeMuji,
    });
    expect($mucCallParticipants.get()).toEqual({
      "room@muc.test": ["alice"],
    });
    expect(mucCallParticipantCount("room@muc.test")).toBe(1);
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

  test("transitioning from active → preparing-only clears the nick", () => {
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
    expect($mucCallParticipants.get()["room@muc.test"]).toBeUndefined();
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

  test("does NOT resolve on a content (active) presence — preparing-only is the trigger", async () => {
    // A content presence is NOT a preparing echo; the waiter must
    // continue to block until the MUC echoes preparing.
    const wait = awaitPreparingEcho("room@muc.test", "alice", 200);
    let resolved = false;
    wait.then(() => {
      resolved = true;
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muji: activeMuji, // active, not preparing
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(resolved).toBe(false);
    // Eventually the 200ms timeout will resolve it; await to clean up.
    await wait;
  });

  test("falls back to timeout when no echo arrives", async () => {
    // The MUC may have dropped the presence; we don't want to
    // hang the call setup forever. 50ms timeout for the test.
    const start = Date.now();
    await awaitPreparingEcho("room@muc.test/nobody", "alice", 50);
    const elapsed = Date.now() - start;
    expect(elapsed).toBeGreaterThanOrEqual(40); // some scheduling slack
    expect(elapsed).toBeLessThan(500);
  });
});
