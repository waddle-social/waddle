import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import type { Agent } from "stanza";
import { queryPersonalMam, queryPersonalMamPage } from "../src/lib/xmpp/dm-history";
import { queryMam, queryMamPage, queryMamThreadPage } from "../src/lib/xmpp/history";
import { useDmMessaging } from "../src/composables/useDmMessaging";
import { useMessaging } from "../src/composables/useMessaging";
import { handlerStubs } from "./helpers/xmpp-client-mock";

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

  test("paged thread history uses the XEP-0313 thread form field and RSM before cursor", async () => {
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
    expect(fields.find((field) => field.name === "{urn:xmpp:mam:2}thread")?.value).toBe("thread-42");
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
  });

  test("applies archived room updates when reactions target an alternate wire id", async () => {
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
    expect(messaging.messages.value[0].reactions).toEqual({ "👍": ["alice"] });
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

  test("preserves GitHub embeds when a correction omits them but URLs still in body", async () => {
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
          body: "check https://github.com/waddle-social/waddle",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message",
          githubEmbeds: [
            { kind: "repo", url: "https://github.com/waddle-social/waddle", owner: "waddle-social", name: "waddle" },
          ],
        },
        {
          id: "edit-1",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "check https://github.com/waddle-social/waddle out!",
          createdAt: "2024-01-01T00:01:00Z",
          type: "message",
          replacesId: "msg-1",
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
      () => { actionError.value = ""; },
    );

    await messaging.loadMessages("w1", "c1");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0].id).toBe("msg-1");
    expect(messaging.messages.value[0].isEdited).toBe(true);
    expect(messaging.messages.value[0].body).toBe("check https://github.com/waddle-social/waddle out!");
    expect(messaging.messages.value[0].githubEmbeds).toEqual([
      { kind: "repo", url: "https://github.com/waddle-social/waddle", owner: "waddle-social", name: "waddle" },
    ]);
  });

  test("drops GitHub embeds when a correction removes the URL from body", async () => {
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
          body: "check https://github.com/waddle-social/waddle",
          createdAt: "2024-01-01T00:00:00Z",
          type: "message",
          githubEmbeds: [
            { kind: "repo", url: "https://github.com/waddle-social/waddle", owner: "waddle-social", name: "waddle" },
          ],
        },
        {
          id: "edit-1",
          roomJid: "general@muc.example.com",
          nick: "bob",
          body: "never mind",
          createdAt: "2024-01-01T00:01:00Z",
          type: "message",
          replacesId: "msg-1",
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
      () => { actionError.value = ""; },
    );

    await messaging.loadMessages("w1", "c1");

    expect(messaging.messages.value).toHaveLength(1);
    expect(messaging.messages.value[0].githubEmbeds).toBeUndefined();
  });
});
