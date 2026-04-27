import { describe, expect, mock, test } from "bun:test";
import { nextTick, ref } from "vue";
import type { Agent } from "stanza";
import type { ExtensionAnnotation } from "../src/lib/chat-ui";
import { queryPersonalMam } from "../src/lib/xmpp/dm-history";
import { queryMam } from "../src/lib/xmpp/history";
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

});
