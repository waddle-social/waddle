import { describe, test, expect, mock } from "bun:test";
import type { Agent } from "stanza";
import { sendCorrection, sendGroupMessage } from "../src/lib/xmpp/messaging";

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
});
