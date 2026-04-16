import { describe, expect, test } from "bun:test";
import { extractMessageExtensions } from "../src/lib/xmpp/message-parsing";
import type { LiveRoomMessage } from "../src/lib/xmpp/types";
import type { WaddleLinkPreview } from "../src/lib/xmpp/extensions/preview";
import type { ReceivedMessage } from "stanza/protocol";

interface WaddleReferenceLike {
  type?: string;
  uri?: string;
  begin?: string;
  end?: string;
  preview?: WaddleLinkPreview;
}

function buildMsg(body: string, references: WaddleReferenceLike[]): ReceivedMessage {
  return {
    type: "groupchat",
    body,
    references,
  } as unknown as ReceivedMessage;
}

function newBase(body: string): LiveRoomMessage {
  return {
    id: "m-1",
    roomJid: "r@example.com",
    nick: "alice",
    body,
    createdAt: "2026-04-15T00:00:00Z",
    type: "message",
  };
}

describe("extractMessageExtensions — link preview", () => {
  test("attaches preview when body[begin..end] equals uri", () => {
    const body = "see https://example.com/a end";
    const preview: WaddleLinkPreview = {
      url: "https://example.com/a",
      title: "T",
    };
    const msg = buildMsg(body, [
      { type: "data", uri: "https://example.com/a", begin: "4", end: "25", preview },
    ]);
    const base = newBase(body);
    extractMessageExtensions(msg, base);
    expect(base.preview).toEqual(preview);
  });

  test("drops preview when body[begin..end] !== uri (anti-spoof)", () => {
    const body = "see https://phish.example/ end";
    const preview: WaddleLinkPreview = { url: "https://legit.example/", title: "Trust me" };
    const msg = buildMsg(body, [
      { type: "data", uri: "https://legit.example/", begin: "4", end: "26", preview },
    ]);
    const base = newBase(body);
    extractMessageExtensions(msg, base);
    expect(base.preview).toBeUndefined();
  });

  test("drops preview when begin/end missing", () => {
    const body = "see https://example.com/a";
    const preview: WaddleLinkPreview = { url: "https://example.com/a", title: "T" };
    const msg = buildMsg(body, [
      { type: "data", uri: "https://example.com/a", preview },
    ]);
    const base = newBase(body);
    extractMessageExtensions(msg, base);
    expect(base.preview).toBeUndefined();
  });

  test("drops preview when uri attr mismatches preview.url", () => {
    const body = "see https://example.com/a end";
    const preview: WaddleLinkPreview = {
      url: "https://different.example/",
      title: "T",
    };
    const msg = buildMsg(body, [
      { type: "data", uri: "https://example.com/a", begin: "4", end: "25", preview },
    ]);
    const base = newBase(body);
    extractMessageExtensions(msg, base);
    expect(base.preview).toBeUndefined();
  });

  test("ignores previews on mention-type references", () => {
    const body = "hi @bob";
    const preview: WaddleLinkPreview = { url: "https://example.com/", title: "X" };
    const msg = buildMsg(body, [
      { type: "mention", uri: "xmpp:bob@example.com", preview },
    ]);
    const base = newBase(body);
    extractMessageExtensions(msg, base);
    expect(base.preview).toBeUndefined();
  });

  test("takes first valid preview when multiple references carry previews", () => {
    const body = "one https://a.example/ two https://b.example/ end";
    const pa: WaddleLinkPreview = { url: "https://a.example/", title: "A" };
    const pb: WaddleLinkPreview = { url: "https://b.example/", title: "B" };
    const msg = buildMsg(body, [
      { type: "data", uri: "https://a.example/", begin: "4", end: "22", preview: pa },
      { type: "data", uri: "https://b.example/", begin: "27", end: "45", preview: pb },
    ]);
    const base = newBase(body);
    extractMessageExtensions(msg, base);
    expect(base.preview?.title).toBe("A");
  });

  test("skips an invalid first preview but accepts a later valid one", () => {
    const body = "x https://good.example/ y";
    const bad: WaddleLinkPreview = { url: "https://good.example/", title: "B" };
    const msg = buildMsg(body, [
      // offsets are wrong for this one
      { type: "data", uri: "https://good.example/", begin: "0", end: "1", preview: bad },
      { type: "data", uri: "https://good.example/", begin: "2", end: "23", preview: bad },
    ]);
    const base = newBase(body);
    extractMessageExtensions(msg, base);
    expect(base.preview?.title).toBe("B");
  });

  test("no preview when references missing", () => {
    const body = "plain message";
    const msg = {
      type: "groupchat",
      body,
    } as unknown as ReceivedMessage;
    const base = newBase(body);
    extractMessageExtensions(msg, base);
    expect(base.preview).toBeUndefined();
  });
});
