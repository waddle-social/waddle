import { describe, test, expect, mock } from "bun:test";
import type { Agent } from "stanza";
import { sendDirectFileMessage, sendDirectMessage } from "../src/lib/xmpp/dm-messaging";

function makeAgent() {
  return {
    sendMessage: mock(() => undefined),
  } as unknown as Agent & {
    sendMessage: ReturnType<typeof mock>;
  };
}

describe("dm messaging", () => {
  test("sends XEP-0447 file-sharing stanza with fallback and OOB", () => {
    const xmpp = makeAgent();
    const fileUrl = "https://xmpp.waddle.social/upload/abc/photo.jpg";

    const messageId = sendDirectFileMessage(xmpp, "bob@waddle.social", fileUrl, {
      name: "photo.jpg",
      mediaType: "image/jpeg",
      size: 12345,
    });

    expect(typeof messageId).toBe("string");
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        id: messageId,
        to: "bob@waddle.social",
        type: "chat",
        body: fileUrl,
        links: [{ url: fileUrl }],
        fallback: { for: "urn:xmpp:sfs:0", body: true },
        processingHints: { store: true },
        fileSharing: expect.objectContaining({
          disposition: "inline",
          name: "photo.jpg",
          mediaType: "image/jpeg",
          size: "12345",
          url: fileUrl,
        }),
      }),
    );
  });
});
