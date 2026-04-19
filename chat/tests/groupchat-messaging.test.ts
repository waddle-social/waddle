import { describe, test, expect, mock } from "bun:test";
import type { Agent } from "stanza";
import {
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

  test("sends XEP-0447 file-only stanza with fallback and OOB", () => {
    const xmpp = makeAgent();
    const fileUrl = "https://xmpp.waddle.social/upload/abc/photo.jpg";

    const messageId = sendGroupMessage(xmpp, "general@muc.waddle.social", "", {
      files: [{ url: fileUrl, name: "photo.jpg", mediaType: "image/jpeg", size: 12345 }],
    });

    expect(typeof messageId).toBe("string");
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(1);
    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call).toMatchObject({
      id: messageId,
      to: "general@muc.waddle.social",
      type: "groupchat",
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

  test("combines user text and attachments in a single room stanza without SFS fallback", () => {
    const xmpp = makeAgent();
    const fileUrl = "https://xmpp.waddle.social/upload/abc/photo.jpg";

    const messageId = sendGroupMessage(xmpp, "general@muc.waddle.social", "check this out", {
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

  test("sends one room stanza with multiple file-sharing attachments", () => {
    const xmpp = makeAgent();
    const urls = [
      "https://xmpp.waddle.social/upload/a/1.jpg",
      "https://xmpp.waddle.social/upload/a/2.jpg",
    ];

    const messageId = sendGroupMessage(xmpp, "general@muc.waddle.social", "gallery", {
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

  test("includes XEP-0448 metadata for encrypted attachments", () => {
    const xmpp = makeAgent();
    const fileUrl = "https://xmpp.waddle.social/upload/abc/photo.jpg.enc";
    const encrypted = {
      cipher: "urn:xmpp:ciphers:aes-256-gcm-nopadding:0",
      keyB64: "a2V5",
      ivB64: "aXY=",
      hashes: [{ algo: "sha-256", valueB64: "aGFzaA==" }],
      sources: [fileUrl],
    } as const;

    sendGroupMessage(xmpp, "general@muc.waddle.social", "", {
      files: [{ url: fileUrl, name: "photo.jpg", mediaType: "image/jpeg", size: 12345, encrypted }],
    });

    const call = (xmpp.sendMessage as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call.fileSharing).toEqual([
      expect.objectContaining({
        url: fileUrl,
        name: "photo.jpg",
      }),
    ]);
    expect(call.encryptedFiles).toEqual([encrypted]);
  });

  test("refuses to send a room stanza with empty body and no attachments", () => {
    const xmpp = makeAgent();
    expect(sendGroupMessage(xmpp, "general@muc.waddle.social", "")).toBeNull();
    expect(xmpp.sendMessage).toHaveBeenCalledTimes(0);
  });

  test("sends XEP-0508 thread-create metadata for forum topics", () => {
    const xmpp = makeAgent();

    sendGroupMessage(xmpp, "roadmap@muc.waddle.social", "Kickoff post", {
      threadCreate: { title: "Roadmap kickoff" },
    });

    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "groupchat",
        body: "Kickoff post",
        threadCreate: { title: "Roadmap kickoff" },
      }),
    );
  });

  test("sends XEP-0508 thread-reply metadata for forum replies", () => {
    const xmpp = makeAgent();

    sendGroupMessage(xmpp, "roadmap@muc.waddle.social", "Count me in", {
      threadId: "topic-1",
      threadReply: { threadId: "topic-1" },
    });

    expect(xmpp.sendMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "groupchat",
        body: "Count me in",
        thread: "topic-1",
        threadReply: { threadId: "topic-1" },
      }),
    );
  });

});
