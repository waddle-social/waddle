import { describe, expect, test } from "bun:test";
import { fromLiveMessage } from "../src/composables/useMessaging";
import type { LiveRoomMessage } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";

const session: WaddleSession = {
  session_id: "sess",
  user_id: "u1",
  username: "bob",
  avatar_url: null,
  xmpp_localpart: "bob",
  jid: "bob@waddle.social",
  xmpp_websocket_url: "wss://example.com/ws",
  is_expired: false,
  expires_at: null,
};

function mkMsg(partial: Partial<LiveRoomMessage> & { id: string }): LiveRoomMessage {
  return {
    roomJid: "lobby@muc.waddle.social",
    nick: "alice",
    body: "",
    createdAt: new Date().toISOString(),
    type: "message",
    ...partial,
  } as LiveRoomMessage;
}

describe("fromLiveMessage reply-preview population", () => {
  test("copies parent body into replyTo.preview when parent lookup resolves", () => {
    const parentBody = "shall we ship?";
    const inbound = mkMsg({
      id: "reply-1",
      body: "yes",
      replyTo: { id: "parent-1", author: "lobby@muc.waddle.social/alice" },
    });

    const tm = fromLiveMessage(session, inbound, (id) =>
      id === "parent-1" ? { body: parentBody } : undefined,
    );

    expect(tm.replyTo).toBeDefined();
    expect(tm.replyTo?.id).toBe("parent-1");
    expect(tm.replyTo?.preview).toBe(parentBody);
    expect(tm.replyTo?.author).toBe("lobby@muc.waddle.social/alice");
  });

  test("omits preview when parent lookup returns undefined", () => {
    const inbound = mkMsg({
      id: "reply-2",
      body: "ok",
      replyTo: { id: "missing", author: "lobby@muc.waddle.social/alice" },
    });

    const tm = fromLiveMessage(session, inbound, () => undefined);

    expect(tm.replyTo?.preview).toBeUndefined();
    expect(tm.replyTo?.id).toBe("missing");
  });

  test("omits preview when no lookup is provided (legacy callers)", () => {
    const inbound = mkMsg({
      id: "reply-3",
      body: "ok",
      replyTo: { id: "parent-3", author: "lobby@muc.waddle.social/alice" },
    });

    const tm = fromLiveMessage(session, inbound);

    expect(tm.replyTo?.preview).toBeUndefined();
  });

  test("does not set replyTo when the inbound message has none", () => {
    const inbound = mkMsg({ id: "plain-1", body: "hi" });

    const tm = fromLiveMessage(session, inbound, () => ({ body: "x" }));

    expect(tm.replyTo).toBeUndefined();
  });
});
