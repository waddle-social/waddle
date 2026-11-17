import { describe, expect, test } from "bun:test";
import { renderRichMessageHtml, richMessageToTiptap, tiptapToRichMessage } from "../src/lib/rich-message";
import { roomJidFromJoinUri, roomLinkCandidates, searchRoomLinks } from "../src/lib/room-links";

describe("room links", () => {
  test("uses a room's bare JID and leaves group DMs out of the picker", () => {
    const [room, ...rest] = roomLinkCandidates([
      { id: "design", jid: "design?notes@rooms.example.com", name: "Design" },
      { id: "private", jid: "private@rooms.example.com", name: "Private chat", isGroupDm: true },
      { id: "missing", name: "Missing address" },
    ]);

    expect(room.label).toBe("#Design");
    expect(room.uri).toBe("xmpp:design%3Fnotes@rooms.example.com?join");
    expect(roomJidFromJoinUri(room.uri)).toBe("design?notes@rooms.example.com");
    expect(rest).toEqual([]);
  });

  test("puts an exact room name or address before partial matches", () => {
    const rooms = roomLinkCandidates([
      { id: "design-research", jid: "design-research@rooms.example.com", name: "Design Research" },
      { id: "design", jid: "design@rooms.example.com", name: "Product Design" },
    ]);

    expect(searchRoomLinks(rooms, "DESIGN").map((room) => room.label)).toEqual([
      "#Product Design", "#Design Research",
    ]);
  });

  test("rejects other XMPP actions, resources, parameters, and malformed addresses", () => {
    for (const uri of [
      "xmpp:design@rooms.example.com?message",
      "xmpp:design@rooms.example.com?join;password=secret",
      "xmpp:design@rooms.example.com?join#fragment",
      "xmpp:design@rooms.example.com/ada?join",
      "xmpp://ada@example.com/design@rooms.example.com?join",
      "xmpp:design%2fada@rooms.example.com?join",
      "xmpp:design%ZZ@rooms.example.com?join",
      "xmpp:design@rooms.example.com?join\n",
    ]) expect(roomJidFromJoinUri(uri)).toBeNull();
  });

  test("carries a room link through the rich message and edit paths", () => {
    const uri = "xmpp:design@rooms.example.com?join";
    const message = tiptapToRichMessage({
      type: "doc",
      content: [{
        type: "paragraph",
        content: [
          { type: "text", text: "📌 See " },
          { type: "text", text: "#design", marks: [{ type: "link", attrs: { href: uri } }] },
        ],
      }],
    });

    expect(message.body).toBe("📌 See #design");
    expect(message.references).toEqual([{ type: "data", uri, begin: 6, end: 13 }]);
    expect(tiptapToRichMessage(richMessageToTiptap(message)).references).toEqual(message.references);
    expect(renderRichMessageHtml(message)).toBe("<p>📌 See #design</p>");
    expect(renderRichMessageHtml({
      ...message,
      roomHref: (jid) => jid === "design@rooms.example.com" ? "/r/design" : null,
    })).toContain('<a href="/r/design" data-room-jid="design@rooms.example.com">#design</a>');
  });
});
