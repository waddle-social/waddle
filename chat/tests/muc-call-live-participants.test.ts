import { afterEach, describe, expect, test } from "bun:test";
import {
  $mucCallLiveParticipants,
  addLiveCallParticipant,
  clearAllLiveCallParticipants,
  clearLiveCallParticipants,
  removeLiveCallParticipant,
  setLiveCallParticipants,
} from "../src/lib/calls/muc-call-live-participants";

const ROOM = "general@muc.test";
const ALICE = "alice@waddle.test/desktop";
const BOB = "bob@waddle.test/web";

afterEach(() => {
  clearAllLiveCallParticipants();
});

describe("muc-call-live-participants", () => {
  test("setLiveCallParticipants replaces the room's identity set and dedupes", () => {
    setLiveCallParticipants(ROOM, [ALICE, BOB, ALICE]);
    expect($mucCallLiveParticipants.get()[ROOM]).toEqual([ALICE, BOB]);

    setLiveCallParticipants(ROOM, [BOB]);
    expect($mucCallLiveParticipants.get()[ROOM]).toEqual([BOB]);
  });

  test("setLiveCallParticipants with an empty list clears the room entry entirely", () => {
    setLiveCallParticipants(ROOM, [ALICE]);
    setLiveCallParticipants(ROOM, []);
    expect($mucCallLiveParticipants.get()[ROOM]).toBeUndefined();
  });

  test("addLiveCallParticipant is idempotent and preserves order", () => {
    addLiveCallParticipant(ROOM, ALICE);
    addLiveCallParticipant(ROOM, ALICE);
    addLiveCallParticipant(ROOM, BOB);
    expect($mucCallLiveParticipants.get()[ROOM]).toEqual([ALICE, BOB]);
  });

  test("removeLiveCallParticipant drops a single identity and deletes the room on last removal", () => {
    setLiveCallParticipants(ROOM, [ALICE, BOB]);
    removeLiveCallParticipant(ROOM, ALICE);
    expect($mucCallLiveParticipants.get()[ROOM]).toEqual([BOB]);

    removeLiveCallParticipant(ROOM, BOB);
    expect($mucCallLiveParticipants.get()[ROOM]).toBeUndefined();
  });

  test("removeLiveCallParticipant is a no-op for unknown identity", () => {
    setLiveCallParticipants(ROOM, [ALICE]);
    removeLiveCallParticipant(ROOM, BOB);
    expect($mucCallLiveParticipants.get()[ROOM]).toEqual([ALICE]);
  });

  test("clearLiveCallParticipants wipes one room without touching others", () => {
    const OTHER_ROOM = "other@muc.test";
    setLiveCallParticipants(ROOM, [ALICE]);
    setLiveCallParticipants(OTHER_ROOM, [BOB]);
    clearLiveCallParticipants(ROOM);
    expect($mucCallLiveParticipants.get()[ROOM]).toBeUndefined();
    expect($mucCallLiveParticipants.get()[OTHER_ROOM]).toEqual([BOB]);
  });

  test("clearAllLiveCallParticipants wipes every room", () => {
    setLiveCallParticipants(ROOM, [ALICE]);
    setLiveCallParticipants("other@muc.test", [BOB]);
    clearAllLiveCallParticipants();
    expect($mucCallLiveParticipants.get()).toEqual({});
  });
});
