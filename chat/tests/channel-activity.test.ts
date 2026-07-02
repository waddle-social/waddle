import { describe, expect, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import { useChannelMessages } from "../src/channels/messages";
import type { WaddleSession } from "../src/lib/server-auth";
import type { LiveRoomMessage, RoomActivityEvent } from "../src/lib/xmpp-client";
import { handlerStubs } from "./helpers/xmpp-client-mock";

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com",
    session_id: "s1",
    user_id: "u1",
    avatar_url: null,
    xmpp_localpart: "alice",
    xmpp_websocket_url: "wss://example.com/xmpp",
    is_expired: false,
    expires_at: null,
    ...partial,
  };
}

describe("channel activity", () => {
  test("does not count off-room self broadcast mentions as Home mention activity", async () => {
    let onActivity: ((event: RoomActivityEvent) => void) | null = null;
    const actionError = ref("");
    const scope = effectScope();
    const messaging = scope.run(() =>
      useChannelMessages(
        ref(session()),
        ref({
          ...handlerStubs(),
          setActivityHandler(handler: (event: RoomActivityEvent) => void) {
            onActivity = handler;
          },
        } as never),
        ref("team"),
        ref("general"),
        ref({ id: "general", name: "General", jid: "general@conference.example.com" }),
        String,
        actionError,
        () => {
          actionError.value = "";
        },
      )
    );

    try {
      if (!messaging) throw new Error("channel messages composable did not initialize");
      await nextTick();

      onActivity?.({
        roomJid: "random@conference.example.com",
        nick: "alice",
        body: "self broadcast",
        broadcastMention: "everyone",
      });

      expect(messaging.activeChannels.value.has("random@conference.example.com")).toBe(true);
      expect(messaging.mentionedChannelCounts.value).toEqual({});
      expect(messaging.lastMentionActivity.value).toBeNull();

      onActivity?.({
        roomJid: "random@conference.example.com",
        nick: "bob",
        body: "wrong alice",
        mentions: ["xmpp:alice@other.example.com"],
      });

      expect(messaging.mentionedChannelCounts.value).toEqual({});
      expect(messaging.lastMentionActivity.value).toBeNull();

      onActivity?.({
        roomJid: "random@conference.example.com",
        nick: "bob",
        body: "personal mention",
        mentions: ["xmpp:alice@example.com"],
      });

      expect(messaging.mentionedChannelCounts.value).toEqual({
        "random@conference.example.com": 1,
      });
      expect(messaging.lastMentionActivity.value?.nick).toBe("bob");

      messaging.clearChannelActivity("random@conference.example.com/bob");
      expect(messaging.activeChannels.value.has("random@conference.example.com")).toBe(false);
      expect(messaging.mentionedChannelCounts.value).toEqual({});
    } finally {
      scope.stop();
    }
  });

  test("counts current-room hidden personal mentions by bare JID, not localpart", async () => {
    let onMessage: ((message: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const scope = effectScope();
    let cleanupDocument: (() => void) | null = null;

    try {
      cleanupDocument = installHiddenDocument();
      const messaging = scope.run(() =>
        useChannelMessages(
          ref(session()),
          ref({
            ...handlerStubs(),
            setMessageHandler(handler: (message: LiveRoomMessage) => void) {
              onMessage = handler;
            },
          } as never),
          ref("team"),
          ref("general"),
          ref({ id: "general", name: "General", jid: "general@conference.example.com" }),
          String,
          actionError,
          () => {
            actionError.value = "";
          },
        )
      );
      if (!messaging) throw new Error("channel messages composable did not initialize");
      await nextTick();

      onMessage?.(roomMessage({
        id: "wrong-domain",
        body: "wrong alice",
        mentions: ["alice@other.example.com"],
      }));

      expect(messaging.mentionedChannelCounts.value).toEqual({});
      expect(messaging.lastMentionActivity.value).toBeNull();

      onMessage?.(roomMessage({
        id: "exact-jid",
        body: "right alice",
        mentions: ["xmpp:alice@example.com"],
      }));

      expect(messaging.mentionedChannelCounts.value).toEqual({
        "general@conference.example.com": 1,
      });
      expect(messaging.lastMentionActivity.value?.body).toBe("right alice");
      const notificationActivity = messaging.pendingNotificationActivities.value.at(-1);
      expect(notificationActivity?.roomJid).toBe("general@conference.example.com");
      expect(notificationActivity?.nick).toBe("bob");
      expect(notificationActivity?.body).toBe("right alice");
      expect(notificationActivity?.mentions).toEqual(["xmpp:alice@example.com"]);
    } finally {
      scope.stop();
      cleanupDocument?.();
    }
  });

  test("records current-room hidden plain messages as foreground notification candidates", async () => {
    let onMessage: ((message: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const scope = effectScope();
    let cleanupDocument: (() => void) | null = null;

    try {
      cleanupDocument = installHiddenDocument();
      const messaging = scope.run(() =>
        useChannelMessages(
          ref(session()),
          ref({
            ...handlerStubs(),
            setMessageHandler(handler: (message: LiveRoomMessage) => void) {
              onMessage = handler;
            },
          } as never),
          ref("team"),
          ref("general"),
          ref({ id: "general", name: "General", jid: "general@conference.example.com" }),
          String,
          actionError,
          () => {
            actionError.value = "";
          },
        )
      );
      if (!messaging) throw new Error("channel messages composable did not initialize");
      await nextTick();

      onMessage?.(roomMessage({
        id: "plain-hidden",
        body: "plain update",
        stanzaId: "stanza-plain-hidden",
      }));

      expect(messaging.pendingNotificationActivities.value.at(-1)).toMatchObject({
        roomJid: "general@conference.example.com",
        nick: "bob",
        body: "plain update",
        stanzaId: "stanza-plain-hidden",
      });
      expect(messaging.mentionedChannelCounts.value).toEqual({});
      expect(messaging.lastMentionActivity.value).toBeNull();
    } finally {
      scope.stop();
      cleanupDocument?.();
    }
  });

  test("records previous-room messages as Home activity when no channel is active", async () => {
    let onMessage: ((message: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const scope = effectScope();
    let cleanupDocument: (() => void) | null = null;
    const messaging = scope.run(() =>
      useChannelMessages(
        ref(session()),
        ref({
          ...handlerStubs(),
          setMessageHandler(handler: (message: LiveRoomMessage) => void) {
            onMessage = handler;
          },
        } as never),
        ref("team"),
        ref(null),
        ref(null),
        String,
        actionError,
        () => {
          actionError.value = "";
        },
      )
    );

    try {
      cleanupDocument = installHiddenDocument();
      if (!messaging) throw new Error("channel messages composable did not initialize");
      await nextTick();

      onMessage?.(roomMessage({
        id: "home-activity",
        body: "right alice from previous room",
        mentions: ["xmpp:alice@example.com"],
      }));

      expect(messaging.messages.value).toEqual([]);
      expect(messaging.activeChannels.value.has("general@conference.example.com")).toBe(true);
      expect(messaging.mentionedChannelCounts.value).toEqual({
        "general@conference.example.com": 1,
      });
      expect(messaging.lastMentionActivity.value?.body).toBe("right alice from previous room");
      const notificationActivity = messaging.pendingNotificationActivities.value.at(-1);
      expect(notificationActivity?.roomJid).toBe("general@conference.example.com");
      expect(notificationActivity?.nick).toBe("bob");
      expect(notificationActivity?.body).toBe("right alice from previous room");
      expect(notificationActivity?.mentions).toEqual(["xmpp:alice@example.com"]);
    } finally {
      scope.stop();
      cleanupDocument?.();
    }
  });

  test("records previous-room hidden plain messages as foreground notification candidates", async () => {
    let onMessage: ((message: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const scope = effectScope();
    let cleanupDocument: (() => void) | null = null;
    const messaging = scope.run(() =>
      useChannelMessages(
        ref(session()),
        ref({
          ...handlerStubs(),
          setMessageHandler(handler: (message: LiveRoomMessage) => void) {
            onMessage = handler;
          },
        } as never),
        ref("team"),
        ref(null),
        ref(null),
        String,
        actionError,
        () => {
          actionError.value = "";
        },
      )
    );

    try {
      cleanupDocument = installHiddenDocument();
      if (!messaging) throw new Error("channel messages composable did not initialize");
      await nextTick();

      onMessage?.(roomMessage({
        id: "home-activity-plain",
        body: "plain update from previous room",
        stanzaId: "stanza-previous-plain",
      }));

      expect(messaging.messages.value).toEqual([]);
      expect(messaging.activeChannels.value.has("general@conference.example.com")).toBe(true);
      expect(messaging.mentionedChannelCounts.value).toEqual({});
      expect(messaging.pendingNotificationActivities.value.at(-1)).toMatchObject({
        roomJid: "general@conference.example.com",
        nick: "bob",
        body: "plain update from previous room",
        stanzaId: "stanza-previous-plain",
      });
    } finally {
      scope.stop();
      cleanupDocument?.();
    }
  });

  test("does not record inactive-room corrections as new Home activity", async () => {
    let onMessage: ((message: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const scope = effectScope();
    const messaging = scope.run(() =>
      useChannelMessages(
        ref(session()),
        ref({
          ...handlerStubs(),
          setMessageHandler(handler: (message: LiveRoomMessage) => void) {
            onMessage = handler;
          },
        } as never),
        ref("team"),
        ref(null),
        ref(null),
        String,
        actionError,
        () => {
          actionError.value = "";
        },
      )
    );

    try {
      if (!messaging) throw new Error("channel messages composable did not initialize");
      await nextTick();

      onMessage?.(roomMessage({
        id: "edited-message",
        body: "edited right alice from previous room",
        mentions: ["xmpp:alice@example.com"],
        replacesId: "original-message",
      }));

      expect(messaging.messages.value).toEqual([]);
      expect(messaging.activeChannels.value.size).toBe(0);
      expect(messaging.mentionedChannelCounts.value).toEqual({});
      expect(messaging.lastMentionActivity.value).toBeNull();
    } finally {
      scope.stop();
    }
  });

  test("records selected-channel messages as Home activity while DM mode owns the chat pane", async () => {
    let onMessage: ((message: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const scope = effectScope();
    const messaging = scope.run(() =>
      useChannelMessages(
        ref(session()),
        ref({
          ...handlerStubs(),
          setMessageHandler(handler: (message: LiveRoomMessage) => void) {
            onMessage = handler;
          },
        } as never),
        ref("team"),
        ref("general"),
        ref({ id: "general", name: "General", jid: "general@conference.example.com" }),
        String,
        actionError,
        () => {
          actionError.value = "";
        },
        undefined,
        ref(false),
      )
    );

    try {
      if (!messaging) throw new Error("channel messages composable did not initialize");
      await nextTick();

      onMessage?.(roomMessage({
        id: "dm-mode-mention",
        body: "right alice while in DM",
        mentions: ["xmpp:alice@example.com"],
      }));

      expect(messaging.messages.value).toEqual([]);
      expect(messaging.activeChannels.value.has("general@conference.example.com")).toBe(true);
      expect(messaging.mentionedChannelCounts.value).toEqual({
        "general@conference.example.com": 1,
      });
      expect(messaging.lastMentionActivity.value?.body).toBe("right alice while in DM");
    } finally {
      scope.stop();
    }
  });

  test("validates live retraction sender identity and moderation stanza target", async () => {
    let onMessage: ((message: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const scope = effectScope();
    const messaging = scope.run(() =>
      useChannelMessages(
        ref(session()),
        ref({
          ...handlerStubs(),
          setMessageHandler(handler: (message: LiveRoomMessage) => void) {
            onMessage = handler;
          },
        } as never),
        ref("team"),
        ref("general"),
        ref({ id: "general", name: "General", jid: "general@conference.example.com" }),
        String,
        actionError,
        () => {
          actionError.value = "";
        },
      )
    );

    try {
      if (!messaging) throw new Error("channel messages composable did not initialize");
      await nextTick();

      messaging.messages.value = [{
        id: "client-id",
        wireIds: ["target-room-stanza"],
        replyableId: "target-room-stanza",
        author: "bob",
        authorJid: "general@conference.example.com/bob",
        authorOccupantJid: "general@conference.example.com/bob",
        body: "keep me",
        createdAt: "2026-05-08T13:00:00Z",
        isSelf: false,
        markup: [{ type: "span", start: 0, end: 4, styles: ["strong"] }],
        references: [{ type: "data", uri: "https://example.com", begin: 0, end: 4 }],
        sharedFiles: [{ url: "https://example.com/file.png", disposition: "inline" }],
        extensionAnnotations: [],
        mentions: ["alice@example.com"],
        broadcastMention: "here",
      }];

      onMessage?.(roomMessage({
        id: "spoofed-retract",
        nick: "mallory",
        fromJid: "general@conference.example.com/mallory",
        body: "",
        retractsId: "target-room-stanza",
      }));
      expect(messaging.messages.value[0]?.isRetracted).toBeUndefined();

      onMessage?.(roomMessage({
        id: "wrong-id-retract",
        nick: "bob",
        fromJid: "general@conference.example.com/bob",
        body: "",
        retractsId: "client-id",
      }));
      expect(messaging.messages.value[0]?.isRetracted).toBeUndefined();

      onMessage?.(roomMessage({
        id: "valid-retract",
        nick: "bob",
        fromJid: "general@conference.example.com/bob",
        body: "",
        retractsId: "target-room-stanza",
      }));
      expect(messaging.messages.value[0]?.isRetracted).toBe(true);
      expect(messaging.messages.value[0]?.markup).toBeUndefined();
      expect(messaging.messages.value[0]?.references).toBeUndefined();
      expect(messaging.messages.value[0]?.sharedFiles).toBeUndefined();
      expect(messaging.messages.value[0]?.extensionAnnotations).toBeUndefined();
      expect(messaging.messages.value[0]?.mentions).toBeUndefined();
      expect(messaging.messages.value[0]?.broadcastMention).toBeUndefined();

      messaging.messages.value = [{
        id: "client-id-2",
        wireIds: ["target-room-stanza-2"],
        replyableId: "target-room-stanza-2",
        author: "carol",
        authorJid: "general@conference.example.com/carol",
        authorOccupantJid: "general@conference.example.com/carol",
        body: "moderate me",
        createdAt: "2026-05-08T13:01:00Z",
        isSelf: false,
      }];

      onMessage?.(roomMessage({
        id: "wrong-target-moderation",
        nick: "unknown",
        fromJid: "general@conference.example.com",
        body: "",
        retractsId: "client-id-2",
        moderationTargetId: "client-id-2",
      }));
      expect(messaging.messages.value[0]?.isRetracted).toBeUndefined();

      onMessage?.(roomMessage({
        id: "spoofed-moderation",
        nick: "mallory",
        fromJid: "general@conference.example.com/mallory",
        body: "",
        retractsId: "target-room-stanza-2",
        moderationTargetId: "target-room-stanza-2",
      }));
      expect(messaging.messages.value[0]?.isRetracted).toBeUndefined();

      onMessage?.(roomMessage({
        id: "valid-moderation",
        nick: "unknown",
        fromJid: "general@conference.example.com",
        body: "",
        retractsId: "target-room-stanza-2",
        moderationTargetId: "target-room-stanza-2",
      }));
      expect(messaging.messages.value[0]?.isRetracted).toBe(true);
    } finally {
      scope.stop();
    }
  });

  test("queues every hidden plain message in a burst for foreground notification", async () => {
    let onMessage: ((message: LiveRoomMessage) => void) | null = null;
    const actionError = ref("");
    const scope = effectScope();
    let cleanupDocument: (() => void) | null = null;

    try {
      cleanupDocument = installHiddenDocument();
      const messaging = scope.run(() =>
        useChannelMessages(
          ref(session()),
          ref({
            ...handlerStubs(),
            setMessageHandler(handler: (message: LiveRoomMessage) => void) {
              onMessage = handler;
            },
          } as never),
          ref("team"),
          ref("general"),
          ref({ id: "general", name: "General", jid: "general@conference.example.com" }),
          String,
          actionError,
          () => {
            actionError.value = "";
          },
        )
      );
      if (!messaging) throw new Error("channel messages composable did not initialize");
      await nextTick();

      onMessage?.(roomMessage({
        id: "burst-1",
        body: "first",
        stanzaId: "stanza-burst-1",
      }));
      onMessage?.(roomMessage({
        id: "burst-2",
        body: "second",
        stanzaId: "stanza-burst-2",
      }));

      expect(messaging.pendingNotificationActivities.value.map((event) => event.stanzaId)).toEqual([
        "stanza-burst-1",
        "stanza-burst-2",
      ]);
    } finally {
      scope.stop();
      cleanupDocument?.();
    }
  });

  test("disconnect clears live Home activity state", async () => {
    let onActivity: ((event: RoomActivityEvent) => void) | null = null;
    const actionError = ref("");
    const scope = effectScope();
    const messaging = scope.run(() =>
      useChannelMessages(
        ref(session()),
        ref({
          ...handlerStubs(),
          setActivityHandler(handler: (event: RoomActivityEvent) => void) {
            onActivity = handler;
          },
        } as never),
        ref("team"),
        ref("general"),
        ref({ id: "general", name: "General", jid: "general@conference.example.com" }),
        String,
        actionError,
        () => {
          actionError.value = "";
        },
      )
    );

    try {
      if (!messaging) throw new Error("channel messages composable did not initialize");
      await nextTick();

      onActivity?.({
        roomJid: "random@conference.example.com",
        nick: "bob",
        body: "personal mention",
        mentions: ["xmpp:alice@example.com"],
      });
      expect(messaging.activeChannels.value.size).toBe(1);
      expect(messaging.mentionedChannelCounts.value).toEqual({
        "random@conference.example.com": 1,
      });

      messaging.disconnect();

      expect(messaging.activeChannels.value.size).toBe(0);
      expect(messaging.mentionedChannelCounts.value).toEqual({});
      expect(messaging.lastMentionActivity.value).toBeNull();
    } finally {
      scope.stop();
    }
  });
});

function roomMessage(partial: Partial<LiveRoomMessage>): LiveRoomMessage {
  return {
    id: "message-1",
    roomJid: "general@conference.example.com",
    nick: "bob",
    body: "hello",
    createdAt: "2026-05-08T13:00:00Z",
    type: "message",
    ...partial,
  };
}

function installHiddenDocument(): () => void {
  const original = Object.getOwnPropertyDescriptor(globalThis, "document");
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    value: {
      hasFocus: () => false,
      visibilityState: "hidden",
    },
  });
  return () => {
    if (original) {
      Object.defineProperty(globalThis, "document", original);
    } else {
      delete (globalThis as { document?: unknown }).document;
    }
  };
}
