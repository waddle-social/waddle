import { describe, expect, test } from "bun:test";

import {
  applyPick,
  resolveBroadcast,
  resolveShow,
  type PresenceMode,
} from "../src/presence/effective-show";

describe("effective-show: resolveShow", () => {
  test("automatic mode resolves to available", () => {
    const mode: PresenceMode = { kind: "automatic" };
    expect(resolveShow(mode)).toBe("available");
  });

  test("a manual away pick resolves to away", () => {
    const mode: PresenceMode = { kind: "manual", status: "away" };
    expect(resolveShow(mode)).toBe("away");
  });

  test("a manual do-not-disturb pick resolves to dnd", () => {
    const mode: PresenceMode = { kind: "manual", status: "dnd" };
    expect(resolveShow(mode)).toBe("dnd");
  });
});

describe("effective-show: applyPick", () => {
  test("picking a status yields a sticky manual mode", () => {
    expect(applyPick("away")).toEqual({ kind: "manual", status: "away" });
  });

  test("reset returns to automatic mode", () => {
    expect(applyPick("reset")).toEqual({ kind: "automatic" });
  });

  test("picking available is a pinned manual mode, distinct from automatic", () => {
    expect(applyPick("available")).toEqual({ kind: "manual", status: "available" });
  });
});

describe("effective-show: resolveBroadcast", () => {
  test("a manual pick broadcasts that status and never an idle stamp", () => {
    // Manual suspends auto-away: even a long idle gap is ignored.
    const longIdle = { show: "xa", idleSince: 123 } as const;
    expect(resolveBroadcast({ kind: "manual", status: "dnd" }, longIdle)).toEqual({
      show: "dnd",
      idleSince: null,
    });
  });

  test("automatic mode adopts the auto-away computation verbatim", () => {
    expect(resolveBroadcast({ kind: "automatic" }, { show: "away", idleSince: 500 })).toEqual({
      show: "away",
      idleSince: 500,
    });
  });
});
