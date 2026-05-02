import { describe, expect, mock, test } from "bun:test";
import { nextTick, ref } from "vue";
import type { Agent } from "stanza";
import type { ExtensionAnnotation } from "../src/lib/chat-ui";
import { queryPersonalMam, queryPersonalMamPage } from "../src/lib/xmpp/dm-history";
import { queryMam, queryMamByThread, queryMamPage, queryMamThreadPage } from "../src/lib/xmpp/history";
import { useDmMessaging } from "../src/composables/useDmMessaging";
import { useMessaging } from "../src/composables/useMessaging";
import { handlerStubs } from "./helpers/xmpp-client-mock";
import type { LiveRoomMessage } from "../src/lib/xmpp/types";

function makeMamAgent(results: unknown[]) {
  return {
    searchHistory: mock(async () => ({ results })),
  } as unknown as Agent & {
    searchHistory: ReturnType<typeof mock>;
  };
}

function makeMamPageAgent(page: { results?: unknown[]; paging?: unknown; complete?: boolean }) {
  return {
    searchHistory: mock(async () => page),
  } as unknown as Agent & {
    searchHistory: ReturnType<typeof mock>;
  };
}

function extensionAnnotation(id = "poll-enrichment"): ExtensionAnnotation {
  return {
    extensionId: "decision-polls",
    annotationId: id,
    surfaceKind: "utility-panel",
    title: "Release vote",
    fields: { capability: "launch" },
    actions: [],
  };
}

describe("MAM history parsing", () => {
  test("requests the latest room history page", async () => {
    const xmpp = makeMamAgent([]);

    await queryMam(xmpp, "room@muc.example.com", 20);

    expect(xmpp.searchHistory).toHaveBeenCalledWith(
      "room@muc.example.com",
      { paging: { max: 20, before: "" } },
    );
  });

  test("paged room history requests latest and older pages with RSM before cursors", async () => {
    const xmpp = makeMamPageAgent({
      results: [],
      paging: { first: "archive-1", last: "archive-20" },
      complete: false,
    });

    const latest = await queryMamPage(xmpp, "room@muc.example.com", 20, { type: "latest" });
    const older = await queryMamPage(xmpp, "room@muc.example.com", 20, {
      type: "before",
      before: "archive-1",
    });

    expect(xmpp.searchHistory.mock.calls[0]?.[1]).toEqual({ paging: { max: 20, before: "" } });
    expect(xmpp.searchHistory.mock.calls[1]?.[1]).toEqual({ paging: { max: 20, before: "archive-1" } });
    expect(latest.firstArchiveId).toBe("archive-1");
    expect(latest.lastArchiveId).toBe("archive-20");
    expect(older.complete).toBe(false);
  });

  test("parses bodyless archived forum thread metadata as durable messages", async () => {
    const xmpp = makeMamAgent([
      {
        id: "topic-archive-id",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:00:00Z") },
          message: {
            id: "topic-wire-id",
            from: "room@muc.example.com/Alice",
            to: "room@muc.example.com",
            type: "groupchat",
            stanzaIds: [{ id: "topic-archive-id", by: "room@muc.example.com" }],
            thread: "topic-archive-id",
            threadCreate: { title: "Roadmap" },
          },
        },
      },
      {
        id: "reply-archive-id",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:01:00Z") },
          message: {
            id: "reply-wire-id",
            from: "room@muc.example.com/Bob",
            to: "room@muc.example.com",
            type: "groupchat",
            stanzaIds: [{ id: "reply-archive-id", by: "room@muc.example.com" }],
            thread: "topic-archive-id",
            threadReply: { threadId: "topic-archive-id" },
          },
        },
      },
    ]);

    const results = await queryMam(xmpp, "room@muc.example.com", 20);

    expect(results).toHaveLength(2);
    expect(results[0]).toMatchObject({
      id: "topic-archive-id",
      body: "",
      threadId: "topic-archive-id",
      forumPostKind: "topic",
      forumTitle: "Roadmap",
      forumThreadTitle: "Roadmap",
    });
    expect(results[1]).toMatchObject({
      id: "reply-archive-id",
      body: "",
      threadId: "topic-archive-id",
      forumPostKind: "reply",
    });
  });

  test("parses bodyless archived standard MUC thread metadata as durable messages", async () => {
    const xmpp = makeMamAgent([
      {
        id: "thread-marker-archive-id",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:00:00Z") },
          message: {
            id: "thread-marker-wire-id",
            from: "room@muc.example.com/Alice",
            to: "room@muc.example.com",
            type: "groupchat",
            stanzaIds: [{ id: "thread-marker-archive-id", by: "room@muc.example.com" }],
            thread: "thread-root",
            parentThread: "parent-root",
          },
        },
      },
    ]);

    const results = await queryMam(xmpp, "room@muc.example.com", 20);

    expect(results).toHaveLength(1);
    expect(results[0]).toMatchObject({
      id: "thread-marker-archive-id",
      body: "",
      threadId: "thread-root",
      parentThreadId: "parent-root",
    });
  });

  test("parses archived room reactions, corrections, and retractions with original stanza IDs", async () => {
    const xmpp = makeMamAgent([
      {
        id: "archive-msg-1",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:00:00Z") },
          message: {
            id: "msg-1",
            from: "room@muc.example.com/Alice",
            to: "room@muc.example.com",
            type: "groupchat",
            body: "hello",
          },
        },
      },
      {
        id: "archive-reaction-1",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:01:00Z") },
          message: {
            id: "reaction-1",
            from: "room@muc.example.com/Bob",
            to: "room@muc.example.com",
            type: "groupchat",
            muc: { item: { jid: "bob@example.com/mobile" } },
            reactions: { id: "msg-1", items: ["👍"] },
          },
        },
      },
      {
        id: "archive-edit-1",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:02:00Z") },
          message: {
            id: "edit-1",
            from: "room@muc.example.com/Alice",
            to: "room@muc.example.com",
            type: "groupchat",
            body: "hello, edited",
            replace: "msg-1",
          },
        },
      },
      {
        id: "archive-retract-1",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:03:00Z") },
          message: {
            id: "retract-1",
            from: "room@muc.example.com/Alice",
            to: "room@muc.example.com",
            type: "groupchat",
            retract: { id: "msg-1" },
          },
        },
      },
    ]);

    const results = await queryMam(xmpp, "room@muc.example.com", 20);

    expect(results).toHaveLength(4);
    expect(results[0].id).toBe("msg-1");
    expect(results[1]._reactionTarget).toBe("msg-1");
    expect(results[1]._reactionEmojis).toEqual(["👍"]);
    expect(results[1]._reactionSenderId).toBe("bob@example.com");
    expect(results[2].id).toBe("edit-1");
    expect(results[2].replacesId).toBe("msg-1");
    expect(results[3].id).toBe("retract-1");
    expect(results[3].retractsId).toBe("msg-1");
  });

  test("preserves real MUC author JID from archived muc#user item", async () => {
    const xmpp = makeMamAgent([
      {
        id: "archive-msg-1",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:00:00Z") },
          message: {
            id: "msg-1",
            from: "room@muc.example.com/randax",
            to: "room@muc.example.com",
            type: "groupchat",
            body: "hello",
            muc: { item: { jid: "randax@example.com/mobile" } },
          },
        },
      },
    ]);

    const results = await queryMam(xmpp, "room@muc.example.com", 20);

    expect(results).toHaveLength(1);
    expect(results[0].nick).toBe("randax");
    expect(results[0].authorRealJid).toBe("randax@example.com");
  });

  test("parses archived direct-message reactions, corrections, and retractions with original stanza IDs", async () => {
    const xmpp = makeMamAgent([
      {
        id: "archive-msg-1",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:00:00Z") },
          message: {
            id: "msg-1",
            from: "bob@example.com/mobile",
            to: "alice@example.com/desktop",
            type: "chat",
            body: "hey",
          },
        },
      },
      {
        id: "archive-reaction-1",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:01:00Z") },
          message: {
            id: "reaction-1",
            from: "bob@example.com/mobile",
            to: "alice@example.com/desktop",
            type: "chat",
            reactions: { id: "msg-1", items: ["🔥"] },
          },
        },
      },
      {
        id: "archive-edit-1",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:02:00Z") },
          message: {
            id: "edit-1",
            from: "bob@example.com/mobile",
            to: "alice@example.com/desktop",
            type: "chat",
            body: "hey there",
            replace: "msg-1",
          },
        },
      },
      {
        id: "archive-retract-1",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:03:00Z") },
          message: {
            id: "retract-1",
            from: "bob@example.com/mobile",
            to: "alice@example.com/desktop",
            type: "chat",
            retract: { id: "msg-1" },
          },
        },
      },
    ]);

    const results = await queryPersonalMam(xmpp, "alice@example.com", "bob@example.com", 20);

    expect(results).toHaveLength(4);
    expect(results[0].id).toBe("msg-1");
    expect(results[1]._reactionTarget).toBe("msg-1");
    expect(results[1]._reactionEmojis).toEqual(["🔥"]);
    expect(results[2].replacesId).toBe("msg-1");
    expect(results[3].retractsId).toBe("msg-1");
  });

  test("requests the latest DM history page", async () => {
    const xmpp = makeMamAgent([]);

    await queryPersonalMam(xmpp, "alice@example.com", "bob@example.com", 20);

    expect(xmpp.searchHistory).toHaveBeenCalledWith(
      "alice@example.com",
      expect.objectContaining({
        paging: { max: 20, before: "" },
      }),
    );
  });

  test("paged DM history keeps the with filter and uses RSM before cursors", async () => {
    const xmpp = makeMamPageAgent({
      results: [],
      paging: { first: "dm-1", last: "dm-20" },
      complete: true,
    });

    const page = await queryPersonalMamPage(
      xmpp,
      "alice@example.com",
      "bob@example.com",
      20,
      { type: "before", before: "dm-1" },
    );

    const callArgs = xmpp.searchHistory.mock.calls[0];
    expect(callArgs?.[0]).toBe("alice@example.com");
    expect((callArgs?.[1] as { paging?: unknown })?.paging).toEqual({ max: 20, before: "dm-1" });
    const fields =
      ((callArgs?.[1] as { form?: { fields?: { name: string; value?: string }[] } })?.form?.fields) ?? [];
    expect(fields.find((field) => field.name === "with")?.value).toBe("bob@example.com");
    expect(page.firstArchiveId).toBe("dm-1");
    expect(page.complete).toBe(true);
  });

  test("paged thread history uses the Waddle MAM thread form field and RSM before cursor", async () => {
    const xmpp = makeMamPageAgent({ results: [], paging: { first: "thread-1" }, complete: false });

    await queryMamThreadPage(
      xmpp,
      "room@muc.example.com",
      "thread-42",
      20,
      { type: "before", before: "thread-1" },
    );

    const callArgs = xmpp.searchHistory.mock.calls[0];
    expect((callArgs?.[1] as { paging?: unknown })?.paging).toEqual({ max: 20, before: "thread-1" });
    const fields =
      ((callArgs?.[1] as { form?: { fields?: { name: string; value?: string }[] } })?.form?.fields) ?? [];
    expect(fields.find((field) => field.name === "{urn:waddle:mam-thread:0}thread")?.value).toBe("thread-42");
    expect(fields.find((field) => field.name === "{urn:xmpp:mam:2}thread")).toBeUndefined();
  });

  test("thread backfill assigns legacy reply without thread only for requested thread", async () => {
    const xmpp = makeMamAgent([
      {
        id: "archive-reply-1",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:00:00Z") },
          message: {
            id: "reply-1",
            from: "room@muc.example.com/Waddle",
            to: "room@muc.example.com",
            type: "groupchat",
            body: "provider answer",
            reply: { id: "thread-42" },
          },
        },
      },
    ]);

    const threadResults = await queryMamByThread(xmpp, "room@muc.example.com", "thread-42", 20);
    const roomResults = await queryMam(makeMamAgent([
      {
        id: "archive-reply-1",
        item: {
          delay: { timestamp: new Date("2024-01-01T00:00:00Z") },
          message: {
            id: "reply-1",
            from: "room@muc.example.com/Waddle",
            to: "room@muc.example.com",
            type: "groupchat",
            body: "provider answer",
            reply: { id: "thread-42" },
          },
        },
      },
    ]), "room@muc.example.com", 20);

    expect(threadResults[0].replyTo?.id).toBe("thread-42");
    expect(threadResults[0].threadId).toBe("thread-42");
    expect(roomResults[0].replyTo?.id).toBe("thread-42");
    expect(roomResults[0].threadId).toBeUndefined();
  });

  // Reconnect catch-up: an iPhone/Safari session that drops its websocket
  // and resumes must be able to request "everything after my last-seen stanza"
  // via XEP-0313 §4.1.5's `start` form field. These tests fix the shape of
  // the outgoing MAM query so the client can drive MAM-on-reconnect.
  test("MAM DM query includes a `start` form field when a since cursor is given", async () => {
    const xmpp = makeMamAgent([]);

    await queryPersonalMam(
      xmpp,
      "alice@example.com",
      "bob@example.com",
      20,
      "2024-01-02T03:04:05.000Z",
    );

    const callArgs = xmpp.searchHistory.mock.calls[0];
    expect(callArgs?.[0]).toBe("alice@example.com");
    const form = (callArgs?.[1] as { form?: { fields?: { name: string; value?: string }[] } })?.form;
    const fields = form?.fields ?? [];
    const startField = fields.find((f) => f.name === "start");
    expect(startField?.value).toBe("2024-01-02T03:04:05.000Z");
    // Catch-up page wants oldest-first with no `before`, not newest-first.
    expect(
      (callArgs?.[1] as { paging?: { before?: string } })?.paging?.before,
    ).toBeUndefined();
  });

  test("MAM DM catch-up query includes an `end` form field when until is given", async () => {
    const xmpp = makeMamAgent([]);
    await queryPersonalMam(
      xmpp,
      "alice@example.com",
      "bob@example.com",
      20,
      "2024-01-02T03:04:05.000Z",
      "2024-01-02T03:05:00.000Z",
    );
    const callArgs = xmpp.searchHistory.mock.calls[0];
    const fields =
      ((callArgs?.[1] as { form?: { fields?: { name: string; value?: string }[] } })?.form?.fields) ?? [];
    const endField = fields.find((f) => f.name === "end");
    expect(endField?.value).toBe("2024-01-02T03:05:00.000Z");
  });

  test("MAM DM query omits `start` when no since cursor is given", async () => {
    const xmpp = makeMamAgent([]);
    await queryPersonalMam(xmpp, "alice@example.com", "bob@example.com", 20);

    const callArgs = xmpp.searchHistory.mock.calls[0];
    const fields =
      ((callArgs?.[1] as { form?: { fields?: { name: string }[] } })?.form?.fields) ?? [];
    expect(fields.find((f) => f.name === "start")).toBeUndefined();
  });

  test("MAM room query includes a `start` form field when a since cursor is given", async () => {
    const xmpp = makeMamAgent([]);

    await queryMam(xmpp, "general@muc.example.com", 20, "2024-01-02T03:04:05.000Z");

    const callArgs = xmpp.searchHistory.mock.calls[0];
    expect(callArgs?.[0]).toBe("general@muc.example.com");
    const form = (callArgs?.[1] as { form?: { fields?: { name: string; value?: string }[] } })?.form;
    const fields = form?.fields ?? [];
    const formType = fields.find((f) => f.name === "FORM_TYPE");
    expect(formType?.value).toBe("urn:xmpp:mam:2");
    const startField = fields.find((f) => f.name === "start");
    expect(startField?.value).toBe("2024-01-02T03:04:05.000Z");
  });

  test("MAM room catch-up query includes an `end` form field when until is given", async () => {
    const xmpp = makeMamAgent([]);
    await queryMam(
      xmpp,
      "general@muc.example.com",
      20,
      "2024-01-02T03:04:05.000Z",
      "2024-01-02T03:05:00.000Z",
    );
    const callArgs = xmpp.searchHistory.mock.calls[0];
    const fields =
      ((callArgs?.[1] as { form?: { fields?: { name: string; value?: string }[] } })?.form?.fields) ?? [];
    const endField = fields.find((f) => f.name === "end");
    expect(endField?.value).toBe("2024-01-02T03:05:00.000Z");
  });
});

describe("MAM history application", () => {
  test("loads bodyless standard MUC thread rows without forum decoration", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    const xmppClient = ref({
      ...handlerStubs(),
      queryMam: mock(async () => [
        {
          id: "thread-marker-archive-id",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "",
          createdAt: "2024-01-01T00:01:00Z",
          type: "message",
          threadId: "thread-root",
          parentThreadId: "parent-root",
        },
      ] satisfies LiveRoomMessage[]),
    } as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("w1", "c1");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0]).toMatchObject({
      id: "thread-marker-archive-id",
      body: "",
      threadId: "thread-root",
      parentThreadId: "parent-root",
    });
    expect(messaging.messages.value[0].forumPostKind).toBeUndefined();
    expect(messaging.messages.value[0].forumThreadTitle).toBeUndefined();
  });

  test("loads bodyless forum metadata rows from MAM into the timeline", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    const xmppClient = ref({
      ...handlerStubs(),
      queryMam: mock(async () => [
        {
          id: "topic-archive-id",
          roomJid: "general@muc.example.com",
          nick: "alice",
          body: "",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message",
          threadId: "topic-archive-id",
          forumPostKind: "topic",
          forumTitle: "Roadmap",
          forumThreadTitle: "Roadmap",
        },
        {
          id: "reply-archive-id",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "",
          createdAt: "2024-01-01T00:01:00Z",
          type: "message",
          threadId: "topic-archive-id",
          forumPostKind: "reply",
        },
      ] satisfies LiveRoomMessage[]),
    } as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "forum" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("w1", "c1");

    expect(messaging.messages.value.map((message) => message.id)).toEqual([
      "topic-archive-id",
      "reply-archive-id",
    ]);
    expect(messaging.messages.value[0]).toMatchObject({
      threadId: "topic-archive-id",
      forumPostKind: "topic",
      forumTitle: "Roadmap",
    });
    expect(messaging.messages.value[1]).toMatchObject({
      threadId: "topic-archive-id",
      forumPostKind: "reply",
      forumThreadTitle: "Roadmap",
    });
  });

  test("labels bodyless forum replies when MAM omits the topic root", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    const xmppClient = ref({
      ...handlerStubs(),
      queryMam: mock(async () => [
        {
          id: "reply-archive-id",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "",
          createdAt: "2024-01-01T00:01:00Z",
          type: "message",
          threadId: "topic-archive-id",
          forumPostKind: "reply",
        },
      ] satisfies LiveRoomMessage[]),
    } as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "forum" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("w1", "c1");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0]).toMatchObject({
      id: "reply-archive-id",
      body: "",
      threadId: "topic-archive-id",
      forumPostKind: "reply",
      forumThreadTitle: "Thread topic-archive-id",
    });
  });

  test("labels inferred bodyless thread replies when MAM omits forum reply metadata", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    const xmppClient = ref({
      ...handlerStubs(),
      queryMam: mock(async () => [
        {
          id: "reply-archive-id",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "",
          createdAt: "2024-01-01T00:01:00Z",
          type: "message",
          threadId: "topic-archive-id",
        },
      ] satisfies LiveRoomMessage[]),
    } as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "forum" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("w1", "c1");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0]).toMatchObject({
      id: "reply-archive-id",
      body: "",
      threadId: "topic-archive-id",
      forumPostKind: "reply",
      forumThreadTitle: "Thread topic-archive-id",
    });
  });

  test("applies archived room updates onto the original timeline message", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    const xmppClient = ref({
      ...handlerStubs(),
      queryMam: mock(async () => [
        {
          id: "msg-1",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "hello",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message",
          reactionTargetId: "msg-1",
          extensionAnnotations: [extensionAnnotation()],
        },
        {
          id: "edit-1",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "hello, edited",
          createdAt: "2024-01-01T00:01:00Z",
          type: "message",
          replacesId: "msg-1",
        },
        {
          id: "reaction-1",
          roomJid: "general@muc.example.com",
          nick: "alice",
          body: "",
          createdAt: "2024-01-01T00:02:00Z",
          type: "subject",
          _reactionTarget: "msg-1",
          _reactionEmojis: ["👍"],
          _reactionSenderId: "alice@example.com",
        },
        {
          id: "retract-1",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "",
          createdAt: "2024-01-01T00:03:00Z",
          type: "message",
          retractsId: "msg-1",
        },
      ]),
    } as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("w1", "c1");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0].id).toBe("msg-1");
    expect(messaging.messages.value[0].body).toBe("");
    expect(messaging.messages.value[0].isEdited).toBe(true);
    expect(messaging.messages.value[0].isRetracted).toBe(true);
    expect(messaging.messages.value[0].reactions).toEqual({ "👍": ["alice"] });
    expect(messaging.messages.value[0].extensionAnnotations).toBeUndefined();
  });

  test("thread backfill merges missing metadata into an existing room MAM reply", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    const xmppClient = ref({
      ...handlerStubs(),
      queryMam: mock(async () => [
        {
          id: "thread-42",
          reactionTargetId: "thread-42",
          roomJid: "general@muc.example.com",
          nick: "alice",
          body: "topic root",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message",
        },
        {
          id: "stable-reply-1",
          wireIds: ["reply-1"],
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "room copy",
          createdAt: "2024-01-01T00:01:00Z",
          type: "message",
          replyTo: { id: "thread-42" },
        },
        {
          id: "edit-1",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "room copy, edited",
          createdAt: "2024-01-01T00:02:00Z",
          type: "message",
          replacesId: "stable-reply-1",
        },
      ]),
      queryMamByThread: mock(async () => [
        {
          id: "reply-1",
          wireIds: ["stable-reply-1"],
          reactionTargetId: "reply-1",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "thread copy should not replace existing body",
          createdAt: "2024-01-01T00:09:00Z",
          type: "message",
          replyTo: { id: "thread-42", author: "alice" },
          threadId: "thread-42",
          parentThreadId: "parent-thread",
        },
        {
          id: "reaction-1",
          roomJid: "general@muc.example.com",
          nick: "dave",
          body: "",
          createdAt: "2024-01-01T00:10:00Z",
          type: "subject",
          _reactionTarget: "reply-1",
          _reactionEmojis: ["🔥"],
          _reactionSenderId: "dave@example.com",
        },
      ]),
    } as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("w1", "c1");
    const loadedReply = messaging.messages.value.find((message) => message.id === "stable-reply-1");

    expect(messaging.messages.value).toHaveLength(2);
    expect(loadedReply?.threadId).toBeUndefined();
    expect(loadedReply?.body).toBe("room copy, edited");
    expect(loadedReply?.isEdited).toBe(true);
    expect(loadedReply?.reactions).toBeUndefined();

    await messaging.backfillThread("thread-42");
    const mergedReply = messaging.messages.value.find((message) => message.id === "stable-reply-1");

    expect(messaging.messages.value).toHaveLength(2);
    expect(mergedReply?.threadId).toBe("thread-42");
    expect(mergedReply?.parentThreadId).toBe("parent-thread");
    expect(mergedReply?.wireIds).toContain("reply-1");
    expect(mergedReply?.replyTo).toEqual({
      id: "thread-42",
      author: "alice",
      preview: "topic root",
    });
    expect(mergedReply?.body).toBe("room copy, edited");
    expect(mergedReply?.createdAt).toBe("2024-01-01T00:01:00Z");
    expect(mergedReply?.isEdited).toBe(true);
    expect(mergedReply?.reactions).toEqual({ "🔥": ["dave"] });
  });

  test("ignores archived room reactions that target an alternate wire id", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    const xmppClient = ref({
      ...handlerStubs(),
      queryMam: mock(async () => [
        {
          id: "stable-msg-1",
          wireIds: ["echo-msg-1", "client-msg-1"],
          reactionTargetId: "stable-msg-1",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "hello",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message",
        },
        {
          id: "reaction-1",
          roomJid: "general@muc.example.com",
          nick: "alice",
          body: "",
          createdAt: "2024-01-01T00:02:00Z",
          type: "subject",
          _reactionTarget: "client-msg-1",
          _reactionEmojis: ["👍"],
        },
      ]),
    } as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("w1", "c1");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0].id).toBe("stable-msg-1");
    expect(messaging.messages.value[0].reactions).toBeUndefined();
  });

  test("archived room reactions replace a sender's previous reaction set", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    const xmppClient = ref({
      ...handlerStubs(),
      queryMam: mock(async () => [
        {
          id: "msg-1",
          reactionTargetId: "msg-1",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "hello",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message",
        },
        {
          id: "reaction-1",
          roomJid: "general@muc.example.com",
          nick: "alice",
          body: "",
          createdAt: "2024-01-01T00:01:00Z",
          type: "subject",
          _reactionTarget: "msg-1",
          _reactionEmojis: ["👍"],
          _reactionSenderId: "alice@example.com",
        },
        {
          id: "reaction-2",
          roomJid: "general@muc.example.com",
          nick: "alice",
          body: "",
          createdAt: "2024-01-01T00:02:00Z",
          type: "subject",
          _reactionTarget: "msg-1",
          _reactionEmojis: ["🔥"],
          _reactionSenderId: "alice@example.com",
        },
        {
          id: "reaction-3",
          roomJid: "general@muc.example.com",
          nick: "alice",
          body: "",
          createdAt: "2024-01-01T00:03:00Z",
          type: "subject",
          _reactionTarget: "msg-1",
          _reactionEmojis: [],
          _reactionSenderId: "alice@example.com",
        },
        {
          id: "reaction-4",
          roomJid: "general@muc.example.com",
          nick: "alice",
          body: "",
          createdAt: "2024-01-01T00:04:00Z",
          type: "subject",
          _reactionTarget: "msg-1",
          _reactionEmojis: ["👀"],
          _reactionSenderId: "other-alice@example.com",
        },
      ]),
    } as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("w1", "c1");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0].reactions).toEqual({ "👀": ["alice"] });
  });

  test("applies archived DM updates onto the original timeline message", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
    } as never);
    const xmppClient = ref({
      queryPersonalMam: mock(async () => [
        {
          id: "msg-1",
          peerJid: "bob@example.com",
          fromJid: "bob@example.com",
          nick: "bob",
          body: "hey",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message",
          extensionAnnotations: [extensionAnnotation()],
        },
        {
          id: "edit-1",
          peerJid: "bob@example.com",
          fromJid: "bob@example.com",
          nick: "bob",
          body: "hey there",
          createdAt: "2024-01-01T00:01:00Z",
          type: "message",
          replacesId: "msg-1",
        },
        {
          id: "reaction-1",
          peerJid: "bob@example.com",
          fromJid: "bob@example.com",
          nick: "bob",
          body: "",
          createdAt: "2024-01-01T00:02:00Z",
          type: "message",
          _reactionTarget: "msg-1",
          _reactionEmojis: ["🔥"],
        },
        {
          id: "retract-1",
          peerJid: "bob@example.com",
          fromJid: "bob@example.com",
          nick: "bob",
          body: "",
          createdAt: "2024-01-01T00:03:00Z",
          type: "message",
          retractsId: "msg-1",
        },
      ]),
    } as never);
    const actionError = ref("");
    const messaging = useDmMessaging(
      session,
      xmppClient,
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("bob@example.com");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0].id).toBe("msg-1");
    expect(messaging.messages.value[0].body).toBe("");
    expect(messaging.messages.value[0].isEdited).toBe(true);
    expect(messaging.messages.value[0].isRetracted).toBe(true);
    expect(messaging.messages.value[0].reactions).toEqual({ "🔥": ["bob"] });
    expect(messaging.messages.value[0].extensionAnnotations).toBeUndefined();
  });

  test("clears room extension annotations when a live correction omits them", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    let onMessage: ((msg: LiveRoomMessage) => void) | null = null;
    const xmppClient = ref(null as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    xmppClient.value = {
      ...handlerStubs(),
      queryMam: mock(async () => []),
      setMessageHandler(handler: (msg: LiveRoomMessage) => void) {
        onMessage = handler;
      },
    } as never;
    await nextTick();

    messaging.messages.value = [{
      id: "msg-1",
      author: "bob",
      authorJid: "c1@muc.example.com/bob",
      authorOccupantJid: "c1@muc.example.com/bob",
      body: "hello",
      createdAt: "2024-01-01T00:00:00Z",
      isSelf: false,
      extensionAnnotations: [extensionAnnotation()],
    }];

    onMessage?.({
      id: "edit-1",
      roomJid: "c1@muc.example.com",
      nick: "bob",
      body: "hello, edited",
      createdAt: "2024-01-01T00:01:00Z",
      type: "message",
      replacesId: "msg-1",
    });

    expect(messaging.messages.value[0].body).toBe("hello, edited");
    expect(messaging.messages.value[0].isEdited).toBe(true);
    expect(messaging.messages.value[0].extensionAnnotations).toBeUndefined();
  });

  test("live room reactions replace legacy reaction state for the same sender", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    let onReaction: ((event: {
      roomJid: string;
      nick: string;
      messageId: string;
      emojis: string[];
      authorRealJid?: string;
    }) => void) | null = null;
    const xmppClient = ref(null as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    xmppClient.value = {
      ...handlerStubs(),
      queryMam: mock(async () => []),
      setReactionHandler(handler: typeof onReaction) {
        onReaction = handler;
      },
    } as never;
    await nextTick();

    messaging.messages.value = [{
      id: "msg-1",
      reactionTargetId: "msg-1",
      author: "bob",
      authorJid: "c1@muc.example.com/bob",
      authorOccupantJid: "c1@muc.example.com/bob",
      body: "hello",
      createdAt: "2024-01-01T00:00:00Z",
      isSelf: false,
      reactions: { "👍": ["alice"], "✅": ["carol"] },
    }];

    onReaction?.({
      roomJid: "c1@muc.example.com",
      nick: "alice",
      messageId: "msg-1",
      emojis: ["🔥"],
      authorRealJid: "alice@example.com",
    });

    expect(messaging.messages.value[0].reactions).toEqual({ "✅": ["carol"], "🔥": ["alice"] });
    expect(messaging.messages.value[0].reactionSenders).toEqual({
      "✅": { carol: "carol" },
      "🔥": { "alice@example.com": "alice" },
    });
  });

  test("live room reactions replace occupant-key reactions after real JID becomes known", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    let onReaction: ((event: {
      roomJid: string;
      nick: string;
      messageId: string;
      emojis: string[];
      authorRealJid?: string;
    }) => void) | null = null;
    const xmppClient = ref(null as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    xmppClient.value = {
      ...handlerStubs(),
      queryMam: mock(async () => []),
      setReactionHandler(handler: typeof onReaction) {
        onReaction = handler;
      },
    } as never;
    await nextTick();

    messaging.messages.value = [{
      id: "msg-1",
      reactionTargetId: "msg-1",
      author: "bob",
      authorJid: "c1@muc.example.com/bob",
      authorOccupantJid: "c1@muc.example.com/bob",
      body: "hello",
      createdAt: "2024-01-01T00:00:00Z",
      isSelf: false,
      reactions: { "👍": ["alice"] },
      reactionSenders: { "👍": { "c1@muc.example.com/alice": "alice" } },
    }];

    onReaction?.({
      roomJid: "c1@muc.example.com",
      nick: "alice",
      messageId: "msg-1",
      emojis: ["🔥"],
      authorRealJid: "alice@example.com",
    });

    expect(messaging.messages.value[0].reactions).toEqual({ "🔥": ["alice"] });
    expect(messaging.messages.value[0].reactionSenders).toEqual({
      "🔥": { "alice@example.com": "alice" },
    });
  });

  test("live room reactions preserve distinct real JID senders with the same nick", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    let onReaction: ((event: {
      roomJid: string;
      nick: string;
      messageId: string;
      emojis: string[];
      authorRealJid?: string;
    }) => void) | null = null;
    const xmppClient = ref(null as never);
    const actionError = ref("");
    const messaging = useMessaging(
      session,
      ref(null),
      xmppClient,
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    xmppClient.value = {
      ...handlerStubs(),
      queryMam: mock(async () => []),
      setReactionHandler(handler: typeof onReaction) {
        onReaction = handler;
      },
    } as never;
    await nextTick();

    messaging.messages.value = [{
      id: "msg-1",
      reactionTargetId: "msg-1",
      author: "bob",
      authorJid: "c1@muc.example.com/bob",
      authorOccupantJid: "c1@muc.example.com/bob",
      body: "hello",
      createdAt: "2024-01-01T00:00:00Z",
      isSelf: false,
      reactions: { "👀": ["alice"] },
      reactionSenders: { "👀": { "other-alice@example.com": "alice" } },
    }];

    onReaction?.({
      roomJid: "c1@muc.example.com",
      nick: "alice",
      messageId: "msg-1",
      emojis: ["🔥"],
      authorRealJid: "alice@example.com",
    });

    expect(messaging.messages.value[0].reactions).toEqual({
      "👀": ["alice"],
      "🔥": ["alice"],
    });
    expect(messaging.messages.value[0].reactionSenders).toEqual({
      "👀": { "other-alice@example.com": "alice" },
      "🔥": { "alice@example.com": "alice" },
    });
  });

  test("clears DM extension annotations when a live correction omits them", () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
    } as never);
    const actionError = ref("");
    const messaging = useDmMessaging(
      session,
      ref({ queryPersonalMam: mock(async () => []) } as never),
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [{
      id: "msg-1",
      author: "bob",
      authorJid: "bob@example.com/web",
      body: "hey",
      createdAt: "2024-01-01T00:00:00Z",
      isSelf: false,
      extensionAnnotations: [extensionAnnotation()],
    }];

    messaging.onIncomingMessage({
      id: "edit-1",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/web",
      nick: "bob",
      body: "hey there",
      createdAt: "2024-01-01T00:01:00Z",
      type: "message",
      replacesId: "msg-1",
    });

    expect(messaging.messages.value[0].body).toBe("hey there");
    expect(messaging.messages.value[0].isEdited).toBe(true);
    expect(messaging.messages.value[0].extensionAnnotations).toBeUndefined();
  });

  test("applies archived DM updates when reactions target an alternate wire id", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
    } as never);
    const xmppClient = ref({
      queryPersonalMam: mock(async () => [
        {
          id: "stable-msg-1",
          wireIds: ["echo-msg-1", "client-msg-1"],
          peerJid: "bob@example.com",
          fromJid: "bob@example.com",
          nick: "bob",
          body: "hey",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message",
        },
        {
          id: "reaction-1",
          peerJid: "bob@example.com",
          fromJid: "bob@example.com",
          nick: "bob",
          body: "",
          createdAt: "2024-01-01T00:02:00Z",
          type: "message",
          _reactionTarget: "client-msg-1",
          _reactionEmojis: ["🔥"],
        },
      ]),
    } as never);
    const actionError = ref("");
    const messaging = useDmMessaging(
      session,
      xmppClient,
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("bob@example.com");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0].id).toBe("stable-msg-1");
    expect(messaging.messages.value[0].reactions).toEqual({ "🔥": ["bob"] });
  });

  test("archived DM reactions replace a sender's previous reaction set", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
    } as never);
    const xmppClient = ref({
      queryPersonalMam: mock(async () => [
        {
          id: "msg-1",
          peerJid: "bob@example.com",
          fromJid: "bob@example.com",
          nick: "bob",
          body: "hey",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message",
        },
        {
          id: "reaction-1",
          peerJid: "bob@example.com",
          fromJid: "bob@example.com",
          nick: "bob",
          body: "",
          createdAt: "2024-01-01T00:01:00Z",
          type: "message",
          _reactionTarget: "msg-1",
          _reactionEmojis: ["🔥"],
        },
        {
          id: "reaction-2",
          peerJid: "bob@example.com",
          fromJid: "bob@example.com",
          nick: "bob",
          body: "",
          createdAt: "2024-01-01T00:02:00Z",
          type: "message",
          _reactionTarget: "msg-1",
          _reactionEmojis: ["🎉"],
        },
      ]),
    } as never);
    const actionError = ref("");
    const messaging = useDmMessaging(
      session,
      xmppClient,
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    await messaging.loadMessages("bob@example.com");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0].reactions).toEqual({ "🎉": ["bob"] });
  });

});
