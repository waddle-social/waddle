import { describe, expect, mock, test } from "bun:test";
import type { Agent } from "stanza";
import { fetchInbox, markInboxRead } from "../src/lib/xmpp/inbox";
import type { InboxEntry } from "../src/lib/xmpp/inbox";
import inboxDefinitions, { NS_INBOX_0 } from "../src/lib/xmpp/extensions/inbox";
import { Registry, XMLElement } from "stanza/jxt";
import {
  createInboxState,
  applyEntry,
  applyEntries,
  markReadInState,
  threadsForRoom,
} from "../src/services/inbox";

function makeAgent(response: unknown) {
  return {
    sendIQ: mock(() => Promise.resolve(response)),
  } as unknown as Agent & { sendIQ: ReturnType<typeof mock> };
}

describe("inbox thread entries", () => {
  test("fetchInbox with threads=true sends room and threads params", async () => {
    const xmpp = makeAgent({
      inbox: {
        totalUnread: 0,
        conversations: [
          {
            partner: "room@muc.example.com",
            kind: "muc",
            lastStanzaId: "sid-100",
            lastUpdated: 1700000,
            unread: 2,
            thread: "thread-42",
            threadTitle: "Getting Started",
            replyCount: 7,
            author: "alice",
          },
        ],
      },
    });

    const result = await fetchInbox(xmpp, { room: "room@muc.example.com", threads: true });
    const call = (xmpp.sendIQ as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call).toMatchObject({
      type: "get",
      inbox: { room: "room@muc.example.com", threads: true },
    });
    expect(result.conversations).toHaveLength(1);
    expect(result.conversations[0]).toMatchObject({
      partner: "room@muc.example.com",
      thread: "thread-42",
      threadTitle: "Getting Started",
      replyCount: 7,
      author: "alice",
      unread: 2,
    });
  });

  test("markInboxRead with threadId sends thread attribute", async () => {
    const xmpp = makeAgent({});
    await markInboxRead(xmpp, "room@muc.example.com", "thread-42");
    const call = (xmpp.sendIQ as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call).toMatchObject({
      type: "set",
      inboxMarkRead: { partner: "room@muc.example.com", thread: "thread-42" },
    });
  });

  test("markInboxRead without threadId omits thread attribute", async () => {
    const xmpp = makeAgent({});
    await markInboxRead(xmpp, "room@muc.example.com");
    const call = (xmpp.sendIQ as ReturnType<typeof mock>).mock.calls[0][0] as Record<string, unknown>;
    expect(call).toMatchObject({
      type: "set",
      inboxMarkRead: { partner: "room@muc.example.com" },
    });
    expect((call as { inboxMarkRead: { thread?: string } }).inboxMarkRead.thread).toBeUndefined();
  });
});

describe("inbox jxt thread wire format", () => {
  function newRegistry(): Registry {
    const r = new Registry();
    r.define(inboxDefinitions);
    return r;
  }

  test("conversation with thread attributes round-trips", () => {
    const r = newRegistry();
    const query = new XMLElement("query", { xmlns: NS_INBOX_0, "total-unread": "1" });
    const conv = new XMLElement("conversation", {
      xmlns: NS_INBOX_0,
      partner: "room@muc.example.com",
      kind: "muc",
      "last-stanza-id": "sid-100",
      "last-updated": "1700000",
      unread: "2",
      thread: "thread-42",
      "thread-title": "Getting Started",
      "reply-count": "7",
      author: "alice",
    });
    query.appendChild(conv);

    const imported = r.import(query) as {
      totalUnread?: number;
      conversations?: Array<Record<string, unknown>>;
    };
    expect(imported.conversations).toHaveLength(1);
    expect(imported.conversations![0]).toMatchObject({
      thread: "thread-42",
      threadTitle: "Getting Started",
      replyCount: 7,
      author: "alice",
    });
  });

  test("query export with room and threads attributes", () => {
    const r = newRegistry();
    const xml = r.export("iq.inbox", {
      room: "room@muc.example.com",
      threads: true,
    } as unknown as Parameters<Registry["export"]>[1]);
    expect(xml).toBeDefined();
    expect(xml!.getAttribute("room")).toBe("room@muc.example.com");
    expect(["1", "true"]).toContain(xml!.getAttribute("threads"));
  });

  test("mark-read export with thread attribute", () => {
    const r = newRegistry();
    const xml = r.export("iq.inboxMarkRead", {
      partner: "room@muc.example.com",
      thread: "thread-42",
    } as unknown as Parameters<Registry["export"]>[1]);
    expect(xml).toBeDefined();
    expect(xml!.getAttribute("partner")).toBe("room@muc.example.com");
    expect(xml!.getAttribute("thread")).toBe("thread-42");
  });
});

describe("inbox state management", () => {
  const channelEntry: InboxEntry = {
    partner: "room@muc.example.com",
    kind: "muc",
    lastStanzaId: "s1",
    lastUpdated: 100,
    unread: 1,
  };

  const threadEntry: InboxEntry = {
    partner: "room@muc.example.com",
    kind: "muc",
    lastStanzaId: "s2",
    lastUpdated: 200,
    unread: 2,
    thread: "t1",
    threadTitle: "Discussion",
    replyCount: 5,
    author: "alice",
  };

  test("applyEntry separates channel and thread entries", () => {
    let state = createInboxState();
    state = applyEntry(state, channelEntry);
    state = applyEntry(state, threadEntry);

    expect(state.channels.size).toBe(1);
    expect(state.threads.size).toBe(1);
    expect(state.channels.get("room@muc.example.com")).toMatchObject({ unread: 1 });
    expect(state.threads.get("room@muc.example.com::t1")).toMatchObject({
      unread: 2,
      threadTitle: "Discussion",
    });
  });

  test("applyEntries processes a batch", () => {
    const state = applyEntries(createInboxState(), [channelEntry, threadEntry]);
    expect(state.channels.size).toBe(1);
    expect(state.threads.size).toBe(1);
  });

  test("markReadInState clears channel unread", () => {
    let state = applyEntry(createInboxState(), channelEntry);
    state = markReadInState(state, "room@muc.example.com");
    expect(state.channels.get("room@muc.example.com")!.unread).toBe(0);
  });

  test("markReadInState clears thread unread independently", () => {
    let state = applyEntries(createInboxState(), [channelEntry, threadEntry]);
    state = markReadInState(state, "room@muc.example.com", "t1");
    // Thread cleared
    expect(state.threads.get("room@muc.example.com::t1")!.unread).toBe(0);
    // Channel unaffected
    expect(state.channels.get("room@muc.example.com")!.unread).toBe(1);
  });

  test("threadsForRoom returns entries sorted newest-first", () => {
    const t2: InboxEntry = {
      partner: "room@muc.example.com",
      kind: "muc",
      lastStanzaId: "s3",
      lastUpdated: 300,
      unread: 0,
      thread: "t2",
      threadTitle: "Another topic",
    };
    const state = applyEntries(createInboxState(), [threadEntry, t2]);
    const threads = threadsForRoom(state, "room@muc.example.com");
    expect(threads).toHaveLength(2);
    expect(threads[0].thread).toBe("t2"); // newer
    expect(threads[1].thread).toBe("t1");
  });

  test("threadsForRoom excludes other rooms", () => {
    const otherRoom: InboxEntry = {
      partner: "other@muc.example.com",
      kind: "muc",
      lastStanzaId: "s4",
      lastUpdated: 400,
      unread: 1,
      thread: "t3",
    };
    const state = applyEntries(createInboxState(), [threadEntry, otherRoom]);
    expect(threadsForRoom(state, "room@muc.example.com")).toHaveLength(1);
    expect(threadsForRoom(state, "other@muc.example.com")).toHaveLength(1);
  });
});
