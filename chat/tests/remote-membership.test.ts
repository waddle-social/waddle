import { describe, expect, test } from "bun:test";
import {
  classifyOwnDeparture,
  MEMBERSHIP_EFFECTS,
  roomActions,
  spaceActions,
  type MembershipEffect,
  type MembershipEnd,
} from "../src/lib/xmpp/remote-membership";

const ENDS: MembershipEnd[] = ["leave-room", "forget-room", "unsubscribe-space", "forget-space", "revoked"];

function endsThat(predicate: (effect: MembershipEffect) => boolean): MembershipEnd[] {
  return ENDS.filter((end) => predicate(MEMBERSHIP_EFFECTS[end]));
}

describe("leave, unsubscribe, forget and revoke", () => {
  test("each end exists for its own record", () => {
    expect(new Set(ENDS.map((end) => MEMBERSHIP_EFFECTS[end].primary)).size).toBe(ENDS.length);
  });

  test("only forgetting removes a bookmark or a list entry, only unfollowing ends a subscription", () => {
    expect(endsThat((effect) => effect.bookmark === "removed")).toEqual(["forget-room"]);
    expect(endsThat((effect) => effect.preference === "removed")).toEqual(["forget-space"]);
    expect(endsThat((effect) => effect.subscription === "ends")).toEqual(["unsubscribe-space"]);
  });

  test("no action of the user's removes a grant the host holds", () => {
    expect(endsThat((effect) => effect.hostGrant === "removed")).toEqual(["revoked"]);
  });

  test("nothing is retried on its own after any of them", () => {
    expect(endsThat((effect) => effect.automaticRetry)).toEqual([]);
  });

  test("leaving keeps the bookmark and only turns autojoin off", () => {
    expect(MEMBERSHIP_EFFECTS["leave-room"]).toMatchObject({ occupancy: "ends", bookmark: "autojoin-off" });
  });

  test("a revocation keeps every record of the user's own and is not retried", () => {
    expect(MEMBERSHIP_EFFECTS.revoked).toEqual({
      primary: "hostGrant",
      occupancy: "ends",
      bookmark: "kept",
      subscription: "kept",
      preference: "kept",
      hostGrant: "removed",
      automaticRetry: false,
    });
  });

  test("unsubscribing a Space leaves its rooms and its list entry", () => {
    expect(MEMBERSHIP_EFFECTS["unsubscribe-space"]).toMatchObject({ occupancy: "kept", bookmark: "kept", preference: "follow-off" });
  });
});

describe("offered actions", () => {
  test("a joined room offers only leave; forget waits until it is left", () => {
    expect(roomActions({ joined: true, bookmarked: true })).toEqual(["leave-room"]);
    expect(roomActions({ joined: false, bookmarked: true })).toEqual(["join", "forget-room"]);
    expect(roomActions({ joined: false, bookmarked: false })).toEqual(["join"]);
  });

  test("a followed Space offers only unfollow; forget waits until it is unfollowed", () => {
    expect(spaceActions({ following: true, listed: true })).toEqual(["unsubscribe-space"]);
    expect(spaceActions({ following: false, listed: true })).toEqual(["follow", "forget-space"]);
    expect(spaceActions({ following: false, listed: false })).toEqual(["follow"]);
  });
});

describe("own departures", () => {
  test("a ban or a removed membership is a revocation", () => {
    expect(classifyOwnDeparture([110, 301])).toBe("revoked");
    expect(classifyOwnDeparture([110, 321])).toBe("revoked");
    expect(classifyOwnDeparture([110, 322])).toBe("revoked");
  });

  test("a lost federation stream or a shutdown is an outage", () => {
    expect(classifyOwnDeparture([110, 333])).toBe("outage");
    expect(classifyOwnDeparture([110, 332])).toBe("outage");
  });

  test("a kick and an ordinary leave are told apart", () => {
    expect(classifyOwnDeparture([110, 307])).toBe("kicked");
    expect(classifyOwnDeparture([110])).toBe("left");
  });

  test("a destroyed room is gone, whatever status codes come with it", () => {
    expect(classifyOwnDeparture([], { destroyed: true })).toBe("destroyed");
    expect(classifyOwnDeparture([110, 333], { destroyed: true })).toBe("destroyed");
  });
});
