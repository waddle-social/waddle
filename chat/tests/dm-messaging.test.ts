import { describe, test, expect, mock } from "bun:test";
import type { Agent } from "stanza";
import { sendDirectMessage } from "../src/lib/xmpp/dm-messaging";

function makeAgent() {
  return {
    sendMessage: mock(() => undefined),
  } as unknown as Agent & {
    sendMessage: ReturnType<typeof mock>;
  };
}

describe("dm messaging", () => {
  test("sends XEP-0447 file-only stanza with fallback and OOB", () => {
    const xmpp = makeAgent();
    const fileUrl = "https://xmpp.waddle.social/upload/abc/photo.jpg";

    const messageId = sendDirectMessage(xmpp, "bob@waddle.social", "", {
      files: [{ url: fileUrl, name: "photo.jpg", mediaType: "image/jpeg", size: 12345 }],
    });

    expect(typeof messageId).toBe("string");
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call).toMatchObject({
      id: messageId,
      to: "bob@waddle.social",
      type: "chat",
      body: fileUrl,
      links: [{ url: fileUrl }],
      fallbacks: [{ for: "urn:xmpp:sfs:0" }],
      processingHints: { store: true },
    });
    expect(Array.isArray(call.fileSharing)).toBe(true);
    expect((call.fileSharing as Array<Record<string, unknown>>)[0]).toMatchObject({
      disposition: "inline",
      name: "photo.jpg",
      mediaType: "image/jpeg",
      size: "12345",
      url: fileUrl,
    });
  });

  test("combines user text and attachments in a single stanza without SFS fallback", () => {
    const xmpp = makeAgent();
    const fileUrl = "https://xmpp.waddle.social/upload/abc/photo.jpg";

    const messageId = sendDirectMessage(xmpp, "bob@waddle.social", "check this out", {
      files: [{ url: fileUrl, name: "photo.jpg", mediaType: "image/jpeg", size: 12345 }],
    });

    expect(typeof messageId).toBe("string");
    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call.body).toBe("check this out");
    expect(call.fallbacks).toBeUndefined();
    expect(call.links).toEqual([{ url: fileUrl }]);
    expect(Array.isArray(call.fileSharing)).toBe(true);
    expect((call.fileSharing as unknown[]).length).toBe(1);
  });

  test("sends one stanza with multiple file-sharing attachments", () => {
    const xmpp = makeAgent();
    const urls = [
      "https://xmpp.waddle.social/upload/a/1.jpg",
      "https://xmpp.waddle.social/upload/a/2.jpg",
    ];

    const messageId = sendDirectMessage(xmpp, "bob@waddle.social", "gallery", {
      files: [
        { url: urls[0], name: "1.jpg", mediaType: "image/jpeg", size: 1 },
        { url: urls[1], name: "2.jpg", mediaType: "image/jpeg", size: 2 },
      ],
    });

    expect(typeof messageId).toBe("string");
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call.links).toEqual([{ url: urls[0] }, { url: urls[1] }]);
    const fs = call.fileSharing as Array<Record<string, unknown>>;
    expect(fs).toHaveLength(2);
    expect(fs[0]).toMatchObject({ url: urls[0] });
    expect(fs[1]).toMatchObject({ url: urls[1] });
  });

  test("refuses to send with empty body and no attachments", () => {
    const xmpp = makeAgent();
    expect(sendDirectMessage(xmpp, "bob@waddle.social", "")).toBeNull();
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(0);
  });
});
