import { afterEach, describe, expect, test } from "bun:test";
import {
  $mucCallParticipants,
  applyMucCallPresence,
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
});
