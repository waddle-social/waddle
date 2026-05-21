import { describe, expect, test } from "bun:test";
import {
  channelIdForRoomJid,
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

describe("channelIdForRoomJid", () => {
  test("resolves a managed-room JID via the local-part convention", () => {
    expect(channelIdForRoomJid([{ id: "general" }], "general@muc.example.com")).toBe(
      "general",
    );
  });

  test("returns null when no matching channel exists", () => {
    expect(channelIdForRoomJid([{ id: "general" }], "design@muc.example.com")).toBeNull();
  });

  test("prefers an explicit channel.jid match over the local-part fallback", () => {
    // A federated channel's MUC localpart can collide with a
    // managed-room id elsewhere — the explicit JID match wins so
    // the federated channel id flows through cleanly instead of
    // falling back to the managed-room parse.
    const channels = [
      { id: "general", jid: "general@muc.example.com" },
      { id: "external", jid: "general@chat.federated.test" },
    ];
    expect(channelIdForRoomJid(channels, "general@chat.federated.test")).toBe(
      "external",
    );
    expect(channelIdForRoomJid(channels, "general@muc.example.com")).toBe("general");
  });

  test("strips a leaked resource before lookup", () => {
    expect(
      channelIdForRoomJid([{ id: "general" }], "general@muc.example.com/alice"),
    ).toBe("general");
  });

  test("returns null for an empty input", () => {
    expect(channelIdForRoomJid([], "")).toBeNull();
  });

  test("is the inverse of roomJidForChannelId for managed rooms", () => {
    const channels = [{ id: "design" }, { id: "general" }];
    const roomJid = roomJidForChannelId(session, channels, "design");
    expect(channelIdForRoomJid(channels, roomJid)).toBe("design");
  });
});
