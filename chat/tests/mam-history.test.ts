import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import type { Agent } from "stanza";
import { queryPersonalMam } from "../src/lib/xmpp/dm-history";
import { queryMam } from "../src/lib/xmpp/history";
import { useDmMessaging } from "../src/composables/useDmMessaging";
import { useMessaging } from "../src/composables/useMessaging";

function makeMamAgent(results: unknown[]) {
  return {
    searchHistory: mock(async () => ({ results })),
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
});

describe("MAM history application", () => {
  test("applies archived room updates onto the original timeline message", async () => {
    const session = ref({
      username: "alice",
      jid: "alice@example.com/desktop",
      domain: "example.com",
    } as never);
    const xmppClient = ref({
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
});
