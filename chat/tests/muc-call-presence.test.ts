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

describe("applyMucCallPresence", () => {
  test("available presence with active extension registers the nick", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_call: { state: "active", call_id: "room@muc.test" },
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
      muc_call: { state: "active", call_id: "room@muc.test" },
    });
    applyMucCallPresence({
      from: "room@muc.test/bob",
      presence_type: "available",
      muc_call: { state: "active", call_id: "room@muc.test" },
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toEqual([
      "alice",
      "bob",
    ]);
  });

  test("inactive extension removes the nick", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_call: { state: "active", call_id: "room@muc.test" },
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_call: { state: "inactive", call_id: "room@muc.test" },
    });
    expect($mucCallParticipants.get()["room@muc.test"]).toBeUndefined();
  });

  test("available presence WITHOUT the extension removes a previously-registered nick", () => {
    // A user who joins the call then sends a fresh presence (e.g.
    // updating their show/status) without the extension means they
    // are no longer advertising the call. Treating this as "left
    // the call" matches the documented permissive parsing.
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_call: { state: "active", call_id: "room@muc.test" },
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
      muc_call: { state: "active", call_id: "room@muc.test" },
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
      muc_call: { state: "active", call_id: "room-a@muc.test" },
    });
    applyMucCallPresence({
      from: "room-b@muc.test/carol",
      presence_type: "available",
      muc_call: { state: "active", call_id: "room-b@muc.test" },
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
      muc_call: { state: "active", call_id: "room@muc.test" },
    });
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_call: { state: "active", call_id: "room@muc.test" },
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
      muc_call: { state: "active", call_id: "room@muc.test" },
    });
    applyMucCallPresence({
      from: "room@muc.test",
      presence_type: "available",
      muc_call: { state: "active", call_id: "room@muc.test" },
    });
    expect($mucCallParticipants.get()).toEqual({});
  });

  test("clearMucCallParticipants drops every tracked room", () => {
    applyMucCallPresence({
      from: "room@muc.test/alice",
      presence_type: "available",
      muc_call: { state: "active", call_id: "room@muc.test" },
    });
    clearMucCallParticipants();
    expect($mucCallParticipants.get()).toEqual({});
  });
});
