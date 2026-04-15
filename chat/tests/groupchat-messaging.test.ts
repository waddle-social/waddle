import { describe, test, expect, mock } from "bun:test";
import type { Agent } from "stanza";
import {
  sendCallInvite,
  sendCorrection,
  sendGroupMessage,
  sendModeration,
  sendReaction,
  sendRetraction,
} from "../src/lib/xmpp/messaging";

function makeAgent() {
  return {
    sendMessage: mock(() => undefined),
  } as unknown as Agent & {
    sendMessage: ReturnType<typeof mock>;
  };
}

describe("groupchat messaging", () => {
  test("requests archival for outbound room messages", () => {
    const xmpp = makeAgent();

    const messageId = sendGroupMessage(xmpp, "general@muc.waddle.social", "hello room");

    expect(typeof messageId).toBe("string");
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        id: messageId,
        to: "general@muc.waddle.social",
        type: "groupchat",
        body: "hello room",
        processingHints: { store: true },
        receipt: { type: "request" },
        marker: { type: "markable" },
      }),
    );
  });

  test("requests archival for outbound room corrections", () => {
    const xmpp = makeAgent();

    const messageId = sendCorrection(xmpp, "general@muc.waddle.social", "updated body", "orig-1");

    expect(typeof messageId).toBe("string");
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        id: messageId,
        to: "general@muc.waddle.social",
        type: "groupchat",
        body: "updated body",
        replace: "orig-1",
        processingHints: { store: true },
      }),
    );
  });

  test("requests archival for outbound room reactions", () => {
    const xmpp = makeAgent();

    sendReaction(xmpp, "general@muc.waddle.social", "msg-1", ["👍"]);

    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        to: "general@muc.waddle.social",
        type: "groupchat",
        reactions: { id: "msg-1", items: ["👍"] },
        processingHints: { store: true },
      }),
    );
  });

  test("requests archival for outbound room retractions", () => {
    const xmpp = makeAgent();

    sendRetraction(xmpp, "general@muc.waddle.social", "msg-1");

    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        to: "general@muc.waddle.social",
        type: "groupchat",
        retract: { id: "msg-1" },
        processingHints: { store: true },
      }),
    );
  });

  test("requests archival for outbound room moderation events", () => {
    const xmpp = makeAgent();

    sendModeration(xmpp, "general@muc.waddle.social", "msg-1", "policy");

    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        to: "general@muc.waddle.social",
        type: "groupchat",
        applyTo: {
          id: "msg-1",
          moderated: { retract: true, reason: "policy" },
        },
        processingHints: { store: true },
      }),
    );
  });

  test("requests archival for outbound room call invites", () => {
    const xmpp = makeAgent();

    const messageId = sendCallInvite(xmpp, "general@muc.waddle.social", {
      video: true,
      sid: "sid-1",
      jingleJid: "sfu.waddle.social",
      externalUri: "xmpp:sfu.waddle.social?jingle;sid=sid-1",
    });

    expect(typeof messageId).toBe("string");
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        id: messageId,
        to: "general@muc.waddle.social",
        type: "groupchat",
        processingHints: { store: true },
      }),
    );
  });
});
