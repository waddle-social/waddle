import { describe, expect, test } from "bun:test";
import { installMockBrowserGlobals } from "./helpers/mock-browser-storage";
import {
  qualifiedMessageKey,
  qualifiedRoomKey,
  qualifiedSpaceKey,
} from "../src/lib/xmpp/qualified-identity";
import { roomKey, setLastSeen } from "../src/lib/last-seen-store";
import { resolveChannelBySlug } from "../src/shell/route-helpers";
import type { ChannelSummary } from "../src/lib/chat-types";

installMockBrowserGlobals();

describe("qualified identity keys", () => {
  test("same room localpart on two domains is distinct, case is folded", () => {
    expect(qualifiedRoomKey("general@muc.a.test")).not.toBe(qualifiedRoomKey("general@muc.b.test"));
    expect(qualifiedRoomKey("General@MUC.A.test/nick")).toBe(qualifiedRoomKey("general@muc.a.test"));
    expect(qualifiedRoomKey("")).toBe("");
  });

  test("same space node on two services is distinct and node is case sensitive", () => {
    expect(qualifiedSpaceKey("spaces.a.test", "main")).not.toBe(qualifiedSpaceKey("spaces.b.test", "main"));
    expect(qualifiedSpaceKey("spaces.a.test", "Main")).not.toBe(qualifiedSpaceKey("spaces.a.test", "main"));
    expect(qualifiedSpaceKey("spaces.a.test", "")).toBe("");
  });

  test("same message id in two rooms or from two authorities is distinct", () => {
    const inA = qualifiedMessageKey("general@muc.a.test", "general@muc.a.test", "1");
    const inB = qualifiedMessageKey("general@muc.b.test", "general@muc.b.test", "1");
    const spoofed = qualifiedMessageKey("general@muc.a.test", "general@muc.b.test", "1");
    expect(new Set([inA, inB, spoofed]).size).toBe(3);
    expect(qualifiedMessageKey("general@muc.a.test", "", "1")).toBe("");
    expect(qualifiedMessageKey("general@muc.a.test", "general@muc.a.test", "")).toBe("");
  });

  test("composite keys cannot be forged by shifting component boundaries", () => {
    expect(qualifiedSpaceKey("a", "b1:c")).not.toBe(qualifiedSpaceKey("a1:b", "c"));
    expect(qualifiedMessageKey("r@a", "r@a", "x:y")).not.toBe(qualifiedMessageKey("r@a", "r@a:x", "y"));
  });
});

describe("last-seen room anchors", () => {
  test("rooms sharing a localpart on different services do not share an anchor", () => {
    setLastSeen(roomKey("general@muc.a.test"), "m-a");
    setLastSeen(roomKey("general@muc.b.test"), "m-b");
    expect(localStorage.getItem(roomKey("general@muc.a.test"))).toBe("m-a");
    expect(localStorage.getItem(roomKey("general@muc.b.test"))).toBe("m-b");
  });

  test("an unaddressed room has no key and writes nothing", () => {
    expect(roomKey("")).toBe("");
    setLastSeen(roomKey(""), "m");
    expect(localStorage.length).toBe(0);
  });

  test("legacy local-id room anchors are discarded, other keys are kept", () => {
    localStorage.setItem("waddle.chat.last-seen.room.general", "legacy");
    localStorage.setItem("waddle.chat.last-seen.dm.bob@a.test", "dm-anchor");
    setLastSeen(roomKey("general@muc.a.test"), "fresh");
    expect(localStorage.getItem("waddle.chat.last-seen.room.general")).toBeNull();
    expect(localStorage.getItem("waddle.chat.last-seen.dm.bob@a.test")).toBe("dm-anchor");
    expect(localStorage.getItem(roomKey("general@muc.a.test"))).toBe("fresh");
  });
});

describe("resolveChannelBySlug with qualified slugs", () => {
  const channels = [
    { id: "general", name: "general", jid: "general@muc.a.test" },
    { id: "general", name: "general", jid: "general@muc.b.test" },
    { id: "other", name: "other", jid: "other@muc.a.test" },
  ] as ChannelSummary[];

  test("a room address resolves the right room even when the local id collides", () => {
    expect(resolveChannelBySlug("general@muc.b.test", channels)?.jid).toBe("general@muc.b.test");
    expect(resolveChannelBySlug("General@MUC.A.test", channels)?.jid).toBe("general@muc.a.test");
  });

  test("the colliding local id stays unresolved and unknown addresses do not match", () => {
    expect(resolveChannelBySlug("general", channels)).toBeUndefined();
    expect(resolveChannelBySlug("general@muc.c.test", channels)).toBeUndefined();
    expect(resolveChannelBySlug("other", channels)?.jid).toBe("other@muc.a.test");
  });
});
