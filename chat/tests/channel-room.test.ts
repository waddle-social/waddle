import { describe, expect, test } from "bun:test";
import {
  isTrustedManagedRoomJid,
  knownChannelIdForRoomJid,
  roomJidForChannelId,
  roomJidForChannelSummary,
} from "../src/lib/channel-room";
import type { WaddleSession } from "../src/lib/server-auth";

const session = {
  jid: "alice@example.com/desktop",
  username: "alice",
  token: "token",
} as WaddleSession;

describe("roomJidForChannelSummary", () => {
  test("uses the discovered channel JID when present", () => {
    expect(roomJidForChannelSummary(session, {
      id: "general",
      jid: "general@conference.example.net",
    })).toBe("general@conference.example.net");
  });

  test("strips any accidental resource from the discovered channel JID", () => {
    expect(roomJidForChannelSummary(session, {
      id: "general",
      jid: "general@conference.example.net/alice",
    })).toBe("general@conference.example.net");
  });

  test("falls back to the managed room JID when discovery has no JID", () => {
    expect(roomJidForChannelSummary(session, { id: "general" })).toBe("general@muc.example.com");
  });

  test("resolves active channel ids through discovered topology", () => {
    expect(roomJidForChannelId(session, [
      { id: "general", jid: "general@conference.example.net" },
      { id: "random", jid: "random@conference.example.net" },
    ], "random")).toBe("random@conference.example.net");
  });

});

describe("knownChannelIdForRoomJid", () => {
  test("prefers exact discovered room JID matches", () => {
    expect(knownChannelIdForRoomJid("general@conference.example.net/alice", [
      { id: "pretty-general", jid: "general@conference.example.net" },
      { id: "general", jid: "general@muc.example.com" },
    ])).toBe("pretty-general");
  });

  test("matches id-only managed channels by room localpart", () => {
    expect(knownChannelIdForRoomJid("General@MUC.Example.com", [
      { id: "general" },
      { id: "random" },
    ], "muc.example.com")).toBe("general");
  });

  test("does not map id-only channels from untrusted room domains", () => {
    expect(knownChannelIdForRoomJid("general@foreign.example.com", [
      { id: "general" },
    ], "muc.example.com")).toBeNull();
  });

  test("does not use id fallback for channels with a mismatched explicit JID", () => {
    expect(knownChannelIdForRoomJid("general@muc.example.com", [
      { id: "general", jid: "general@conference.example.net" },
    ], "muc.example.com")).toBeNull();
  });

  test("does not infer a channel id when the directory has no known match", () => {
    expect(knownChannelIdForRoomJid("orphan@muc.example.com", [])).toBeNull();
    expect(knownChannelIdForRoomJid("orphan@muc.example.com", [
      { id: "general" },
    ], "muc.example.com")).toBeNull();
  });
});

describe("isTrustedManagedRoomJid", () => {
  test("checks managed room domains using bare normalized JIDs", () => {
    expect(isTrustedManagedRoomJid("General@MUC.Example.com/alice", "muc.example.com")).toBe(true);
    expect(isTrustedManagedRoomJid("general@foreign.example.com", "muc.example.com")).toBe(false);
  });
});
