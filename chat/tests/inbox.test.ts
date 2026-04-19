import { describe, expect, mock, test } from "bun:test";
import type { Agent } from "stanza";
import { fetchInbox, markInboxRead } from "../src/lib/xmpp/inbox";
import inboxDefinitions, { NS_INBOX_0 } from "../src/lib/xmpp/extensions/inbox";
import { Registry, XMLElement } from "stanza/jxt";

function makeAgent(response: unknown) {
  return {
    sendIQ: mock(() => Promise.resolve(response)),
  } as unknown as Agent & { sendIQ: ReturnType<typeof mock> };
}

describe("inbox client", () => {
  test("fetchInbox sends IQ-get with since/onlyUnread and parses conversations", async () => {
    const xmpp = makeAgent({
      inbox: {
        totalUnread: 3,
        conversations: [
          {
            partner: "alice@example.com",
            kind: "direct",
            lastStanzaId: "sid-42",
            lastUpdated: 1700000,
            unread: 2,
            preview: "hi there",
          },
          {
            partner: "general@conference.example.com",
            kind: "muc",
            lastStanzaId: "sid-99",
            lastUpdated: 1700100,
            unread: 1,
          },
        ],
      },
    });

    const result = await fetchInbox(xmpp, { since: 1700000, onlyUnread: true });
    expect(xmpp.sendIQ).toHaveBeenCalledTimes(1);
    const call = (xmpp.sendIQ as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call).toMatchObject({
      type: "get",
      inbox: { since: 1700000, onlyUnread: true },
    });
    expect(result.totalUnread).toBe(3);
    expect(result.conversations).toHaveLength(2);
    expect(result.conversations[0]).toMatchObject({
      partner: "alice@example.com",
      kind: "direct",
      lastStanzaId: "sid-42",
      lastUpdated: 1700000,
      unread: 2,
      preview: "hi there",
    });
    expect(result.conversations[1]).toMatchObject({
      partner: "general@conference.example.com",
      kind: "muc",
      preview: undefined,
    });
  });

  test("fetchInbox tolerates an empty server response", async () => {
    const xmpp = makeAgent({});
    const result = await fetchInbox(xmpp);
    expect(result.totalUnread).toBe(0);
    expect(result.conversations).toEqual([]);
  });

  test("markInboxRead sends an IQ-set with the partner JID", async () => {
    const xmpp = makeAgent({});
    await markInboxRead(xmpp, "alice@example.com");
    const call = (xmpp.sendIQ as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call).toMatchObject({
      type: "set",
      inboxMarkRead: { partner: "alice@example.com" },
    });
  });
});

describe("inbox jxt extension wire format", () => {
  function newRegistry(): Registry {
    const r = new Registry();
    r.define(inboxDefinitions);
    return r;
  }

  test("query export emits the expected attributes", () => {
    const r = newRegistry();
    const xml = r.export("iq.inbox", {
      since: 1700,
      onlyUnread: true,
    } as unknown as Parameters<Registry["export"]>[1]);
    expect(xml).toBeDefined();
    expect(xml!.name).toBe("query");
    expect(xml!.getNamespace()).toBe(NS_INBOX_0);
    expect(xml!.getAttribute("since")).toBe("1700");
    // jxt's booleanAttribute serialises true as "1" / false as "0";
    // the Rust server accepts both "true" and "1" (xep0430.rs).
    expect(["1", "true"]).toContain(xml!.getAttribute("only-unread"));
  });

  test("conversation list import round-trips", () => {
    const r = newRegistry();
    const query = new XMLElement("query", { xmlns: NS_INBOX_0, "total-unread": "2" });
    const conv = new XMLElement("conversation", {
      xmlns: NS_INBOX_0,
      partner: "alice@example.com",
      kind: "direct",
      "last-stanza-id": "sid-1",
      "last-updated": "1700",
      unread: "2",
    });
    const preview = new XMLElement("preview", { xmlns: NS_INBOX_0 });
    preview.appendChild("hi");
    conv.appendChild(preview);
    query.appendChild(conv);

    const imported = r.import(query) as { totalUnread?: number; conversations?: Array<Record<string, unknown>> };
    expect(imported.totalUnread).toBe(2);
    expect(imported.conversations).toHaveLength(1);
    expect(imported.conversations![0]).toMatchObject({
      partner: "alice@example.com",
      kind: "direct",
      lastStanzaId: "sid-1",
      lastUpdated: 1700,
      unread: 2,
      preview: "hi",
    });
  });

  test("mark-read export emits the partner attribute", () => {
    const r = newRegistry();
    const xml = r.export("iq.inboxMarkRead", { partner: "bob@example.com" } as unknown as Parameters<Registry["export"]>[1]);
    expect(xml).toBeDefined();
    expect(xml!.name).toBe("mark-read");
    expect(xml!.getNamespace()).toBe(NS_INBOX_0);
    expect(xml!.getAttribute("partner")).toBe("bob@example.com");
  });
});
