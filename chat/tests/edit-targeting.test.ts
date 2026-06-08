import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import type { WaddleSession } from "../src/lib/server-auth";
import { useDirectMessages } from "../src/dms/messages";
import { useChannelMessages } from "../src/channels/messages";
import { handlerStubs } from "./helpers/xmpp-client-mock";
import type { ExtensionAnnotation } from "../src/lib/chat-ui";
import type { LiveDmMessage, LiveRoomMessage } from "../src/lib/xmpp-client";

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
    ...partial,
  } as WaddleSession;
}

const extensionAnnotation: ExtensionAnnotation = {
  extensionId: "github",
  annotationId: "github-delivery-1",
  surfaceKind: "message-card",
  title: "GitHub",
  fields: { repository: "waddle-social/waddle", conclusion: "failure" },
  actions: [],
};

describe("edit targeting", () => {
  test("room edits target the XEP-0308 correction id, not the rendered stanza id", async () => {
    const sendCorrection = mock(async () => "edit-1");
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
      ref({ ...handlerStubs(), sendCorrection } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "room-stanza-id",
        wireIds: ["original-message-id"],
        correctionTargetId: "original-message-id",
        author: "alice",
        authorJid: "c1@muc.example.com/alice",
        body: "tyop",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
      },
    ];

    await messaging.editMessage("room-stanza-id", "typo");

    expect(sendCorrection).toHaveBeenCalledWith(
      "w1",
      "c1",
      "typo",
      "original-message-id",
      undefined,
      undefined,
      // XEP-0201 thread arg — absent for a non-threaded target message.
      undefined,
      // Link preview token payload — absent when an edit has no eligible preview.
      undefined,
    );
  });

  test("room corrections from a different occupant do not rewrite the target", async () => {
    let onMessage: ((msg: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
      ref({
        ...handlerStubs(),
        setMessageHandler: (handler: (msg: LiveRoomMessage) => void) => {
          onMessage = handler;
        },
      } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "room-stanza-id",
        wireIds: ["alice-original"],
        correctionTargetId: "alice-original",
        author: "alice",
        authorJid: "c1@muc.example.com/alice",
        body: "alice text",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
      },
    ];

    onMessage?.({
      id: "bob-edit",
      roomJid: "c1@muc.example.com",
      nick: "bob",
      body: "forged edit",
      createdAt: "2024-01-01T00:00:01Z",
      type: "message",
      replacesId: "alice-original",
    });

    expect(messaging.messages.value[0].body).toBe("alice text");
    expect(messaging.messages.value[0].isEdited).toBeUndefined();
  });

  test("room corrections require the same full occupant JID even when real bare JID matches", () => {
    let onMessage: ((msg: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
      ref({
        ...handlerStubs(),
        setMessageHandler: (handler: (msg: LiveRoomMessage) => void) => {
          onMessage = handler;
        },
      } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "room-stanza-id",
        wireIds: ["alice-original"],
        correctionTargetId: "alice-original",
        author: "alice",
        authorJid: "c1@muc.example.com/alice",
        authorRealJid: "alice@example.com",
        body: "alice text",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
      },
    ];

    onMessage?.({
      id: "alice-after-nick-change-edit",
      roomJid: "c1@muc.example.com",
      nick: "alice-renamed",
      authorRealJid: "alice@example.com/phone",
      body: "renamed edit",
      createdAt: "2024-01-01T00:00:01Z",
      type: "message",
      replacesId: "alice-original",
    });

    expect(messaging.messages.value[0].body).toBe("alice text");
    expect(messaging.messages.value[0].isEdited).toBeUndefined();
  });

  test("room corrections apply when a real author JID is known and the occupant JID matches", () => {
    let onMessage: ((msg: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
      ref({
        ...handlerStubs(),
        setMessageHandler: (handler: (msg: LiveRoomMessage) => void) => {
          onMessage = handler;
        },
      } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "room-stanza-id",
        wireIds: ["alice-original"],
        correctionTargetId: "alice-original",
        author: "alice",
        authorJid: "alice@example.com/desktop",
        authorOccupantJid: "c1@muc.example.com/alice",
        authorRealJid: "alice@example.com/desktop",
        body: "alice text",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
      },
    ];

    onMessage?.({
      id: "alice-edit",
      roomJid: "c1@muc.example.com",
      nick: "alice",
      authorRealJid: "alice@example.com/phone",
      body: "edited text",
      createdAt: "2024-01-01T00:00:01Z",
      type: "message",
      replacesId: "alice-original",
    });

    expect(messaging.messages.value[0].body).toBe("edited text");
    expect(messaging.messages.value[0].isEdited).toBe(true);
  });

  test("room corrections clear extension fallback state when the edit has no rich card", () => {
    let onMessage: ((msg: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const messaging = useChannelMessages(
      ref(session()),
      ref(null),
      ref({
        ...handlerStubs(),
        setMessageHandler: (handler: (msg: LiveRoomMessage) => void) => {
          onMessage = handler;
        },
      } as never),
      ref("w1"),
      ref("c1"),
      ref({ id: "c1", name: "general", channel_type: "text" }),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "room-stanza-id",
        wireIds: ["alice-original"],
        correctionTargetId: "alice-original",
        author: "alice",
        authorJid: "c1@muc.example.com/alice",
        body: "GitHub waddle-social/waddle: ci completed with failure",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
        extensionAnnotations: [extensionAnnotation],
        extensionBodyFallback: true,
      },
    ];

    onMessage?.({
      id: "alice-edit",
      roomJid: "c1@muc.example.com",
      nick: "alice",
      body: "plain edit",
      createdAt: "2024-01-01T00:00:01Z",
      type: "message",
      replacesId: "alice-original",
    });

    expect(messaging.messages.value[0].body).toBe("plain edit");
    expect(messaging.messages.value[0].extensionAnnotations).toBeUndefined();
    expect(messaging.messages.value[0].extensionBodyFallback).toBeUndefined();
  });

  test("DM edits target the XEP-0308 correction id, not the rendered stanza id", async () => {
    const sendDmCorrection = mock(async () => "edit-1");
    const actionError = ref("");
    const messaging = useDirectMessages(
      ref(session()),
      ref({ sendDmCorrection } as never),
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "server-stanza-id",
        wireIds: ["original-message-id"],
        correctionTargetId: "original-message-id",
        author: "alice",
        authorJid: "alice@example.com/desktop",
        body: "helo",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
      },
    ];

    await messaging.editMessage("server-stanza-id", "hello");

    expect(sendDmCorrection).toHaveBeenCalledTimes(1);
    const call = sendDmCorrection.mock.calls[0]!;
    expect(call[0]).toBe("bob@example.com");
    expect(call[1]).toBe("hello");
    expect(call[2]).toBe("original-message-id");
  });

  test("DM corrections from a different bare JID do not rewrite the target", () => {
    const actionError = ref("");
    const messaging = useDirectMessages(
      ref(session()),
      ref({} as never),
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "server-stanza-id",
        wireIds: ["alice-original"],
        correctionTargetId: "alice-original",
        author: "alice",
        authorJid: "alice@example.com/desktop",
        body: "alice text",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
      },
    ];

    messaging.onIncomingMessage({
      id: "bob-edit",
      peerJid: "bob@example.com",
      fromJid: "bob@example.com/phone",
      nick: "bob",
      body: "forged edit",
      createdAt: "2024-01-01T00:00:01Z",
      type: "message",
      replacesId: "alice-original",
    } as LiveDmMessage);

    expect(messaging.messages.value[0].body).toBe("alice text");
    expect(messaging.messages.value[0].isEdited).toBeUndefined();
  });

  test("DM corrections apply extension fallback state when the edit has a rich card", () => {
    const actionError = ref("");
    const messaging = useDirectMessages(
      ref(session()),
      ref({} as never),
      ref("bob@example.com"),
      String,
      actionError,
      () => {
        actionError.value = "";
      },
    );

    messaging.messages.value = [
      {
        id: "server-stanza-id",
        wireIds: ["alice-original"],
        correctionTargetId: "alice-original",
        author: "alice",
        authorJid: "alice@example.com/desktop",
        body: "plain text",
        createdAt: "2024-01-01T00:00:00Z",
        isSelf: true,
      },
    ];

    messaging.onIncomingMessage({
      id: "alice-edit",
      peerJid: "bob@example.com",
      fromJid: "alice@example.com/phone",
      nick: "alice",
      body: "GitHub waddle-social/waddle: ci completed with failure",
      createdAt: "2024-01-01T00:00:01Z",
      type: "message",
      replacesId: "alice-original",
      extensionAnnotations: [extensionAnnotation],
      extensionBodyFallback: true,
    } as LiveDmMessage);

    expect(messaging.messages.value[0].body).toBe(
      "GitHub waddle-social/waddle: ci completed with failure",
    );
    expect(messaging.messages.value[0].extensionAnnotations).toEqual([extensionAnnotation]);
    expect(messaging.messages.value[0].extensionBodyFallback).toBe(true);
  });
});
