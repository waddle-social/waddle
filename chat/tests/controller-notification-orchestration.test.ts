import { describe, expect, mock, test } from "bun:test";
import { computed, effectScope, nextTick, ref } from "vue";
import { useChatShellState } from "../src/shell/state";
import { useNotificationOrchestration } from "../src/shell/controllers/use-notification-orchestration";
import type { BrowserXmppClient, ChannelSummary, LiveDmMessage, NotifyMode, RoomActivityEvent } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { NotifySettingsStore } from "../src/lib/notify-settings";
import type { usePushNotifications } from "../src/shell/notifications";
import type { createBrowserMessageTonePlayer } from "../src/shell/audio-alerts";
import type { useWaddleDirectory } from "../src/waddles/directory";
import type { useDirectMessageConversations } from "../src/dms/conversations";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.test",
    session_id: "s1",
    user_id: "u1",
    avatar_url: null,
    xmpp_localpart: "alice",
    xmpp_websocket_url: "wss://example.test/xmpp",
    is_expired: false,
    expires_at: null,
  } as WaddleSession;
}

function activityEvent(partial: Partial<RoomActivityEvent> = {}): RoomActivityEvent {
  return {
    roomJid: "general@muc.example.test",
    nick: "bob",
    body: "hello",
    stanzaId: "st-1",
    ...partial,
  };
}

function dmMessage(partial: Partial<LiveDmMessage> = {}): LiveDmMessage {
  return {
    id: "m1",
    peerJid: "bob@example.test",
    fromJid: "bob@example.test/phone",
    nick: "bob",
    body: "hi",
    createdAt: "2026-01-01T00:00:00.000Z",
    createdAtSource: "server",
    type: "message",
    ...partial,
  } as LiveDmMessage;
}

function makeHarness(options: {
  mode?: NotifyMode;
  doNotDisturb?: boolean;
  sidebarMode?: "channels" | "dms";
  activePeerJid?: string | null;
  channels?: ChannelSummary[];
  ambiguousRoomIds?: string[];
} = {}) {
  const ui = useChatShellState();
  ui.sidebarMode.value = options.sidebarMode ?? "channels";
  const pendingNotificationActivities = ref<RoomActivityEvent[]>([]);
  const showMentionNotification = mock(() => {});
  const showChannelMessageNotification = mock(() => {});
  const showDmNotification = mock(() => {});
  const selectChannelByRoomJid = mock(async () => {});
  const openDm = mock(async () => {});

  const notifications = {
    showMentionNotification,
    showChannelMessageNotification,
    showDmNotification,
    messageSoundsEnabled: ref(false),
    canShowForegroundNotifications: computed(() => true),
    notificationsEnabled: ref(true),
  } as unknown as ReturnType<typeof usePushNotifications>;

  const notifySettings = {
    getMode: () => options.mode ?? "always",
  } as unknown as NotifySettingsStore;

  const messageSound = {
    play: mock(async () => {}),
  } as unknown as ReturnType<typeof createBrowserMessageTonePlayer>;

  const waddles = {
    channels: ref(options.channels ?? [{ id: "general", name: "General", jid: "general@muc.example.test" }]),
  } as unknown as ReturnType<typeof useWaddleDirectory>;

  const dmConversations = {
    activePeerJid: ref<string | null>(options.activePeerJid ?? null),
  } as unknown as ReturnType<typeof useDirectMessageConversations>;

  const scope = effectScope();
  const orchestration = scope.run(() =>
    useNotificationOrchestration({
      ui,
      xmppClient: computed(() => ({
        isRoomIdAmbiguous: (id: string) => options.ambiguousRoomIds?.includes(id) ?? false,
      }) as BrowserXmppClient),
      session: computed(() => session()),
      waddles,
      dmConversations,
      notifications,
      notifySettings,
      messageSound,
      isWindowFocused: ref(true),
      isSelfDoNotDisturb: () => options.doNotDisturb ?? false,
      pendingNotificationActivities,
      selectChannelByRoomJid,
      openDm,
    }),
  )!;

  return {
    ui,
    scope,
    orchestration,
    pendingNotificationActivities,
    showMentionNotification,
    showChannelMessageNotification,
    showDmNotification,
    selectChannelByRoomJid,
    openDm,
  };
}

describe("useNotificationOrchestration channel activity", () => {
  test("drains queued activity into channel notifications with the resolved name", async () => {
    const h = makeHarness();
    h.pendingNotificationActivities.value = [activityEvent()];
    await nextTick();

    expect(h.showChannelMessageNotification).toHaveBeenCalledTimes(1);
    const opts = h.showChannelMessageNotification.mock.calls[0]?.[0] as unknown as {
      channelName: string;
      onNavigate?: (roomJid: string) => void;
    };
    expect(opts.channelName).toBe("General");
    expect(h.pendingNotificationActivities.value).toEqual([]);

    // Navigation retains the full room address for the room-selection boundary.
    opts.onNavigate?.("general@muc.example.test");
    expect(h.selectChannelByRoomJid).toHaveBeenCalledWith("general@muc.example.test");
    h.scope.stop();
  });

  test("uses a discovered custom-service room address for labels and navigation", async () => {
    const roomJid = "general@rooms.other.example.test";
    const h = makeHarness({ channels: [{ id: "general", name: "Other General", jid: roomJid }] });
    h.pendingNotificationActivities.value = [activityEvent({ roomJid })];
    await nextTick();

    const opts = h.showChannelMessageNotification.mock.calls[0]?.[0] as unknown as {
      channelName: string;
      onNavigate?: (roomJid: string) => void;
    };
    expect(opts.channelName).toBe("Other General");
    opts.onNavigate?.(roomJid);
    expect(h.selectChannelByRoomJid).toHaveBeenCalledWith(roomJid);
    h.scope.stop();
  });

  test("unknown foreign rooms cannot borrow a known local room's label or navigation identity", async () => {
    const roomJid = "general@rooms.other.example.test";
    const h = makeHarness();
    h.pendingNotificationActivities.value = [activityEvent({ roomJid })];
    await nextTick();

    const opts = h.showChannelMessageNotification.mock.calls[0]?.[0] as unknown as {
      channelName: string;
      onNavigate?: (roomJid: string) => void;
    };
    expect(opts.channelName).toBe("unknown");
    opts.onNavigate?.(roomJid);
    expect(h.selectChannelByRoomJid).toHaveBeenCalledWith(roomJid);
    h.scope.stop();
  });

  test("colliding bindings do not supply a notification label", async () => {
    const h = makeHarness({ channels: [
      { id: "general", name: "Local General", jid: "general@muc.example.test" },
      { id: "general", name: "Other General", jid: "general@rooms.other.example.test" },
    ] });
    h.pendingNotificationActivities.value = [activityEvent()];
    await nextTick();

    const opts = h.showChannelMessageNotification.mock.calls[0]?.[0] as unknown as { channelName: string };
    expect(opts.channelName).toBe("unknown");
    h.scope.stop();
  });

  test("client quarantine still prevents labeling and navigation after a conflicting catalog entry disappears", async () => {
    const h = makeHarness({ ambiguousRoomIds: ["general"] });
    h.pendingNotificationActivities.value = [activityEvent()];
    await nextTick();

    const opts = h.showChannelMessageNotification.mock.calls[0]?.[0] as unknown as {
      channelName: string;
      onNavigate?: (roomJid: string) => void;
    };
    expect(opts.channelName).toBe("unknown");
    opts.onNavigate?.("general@muc.example.test");
    expect(h.selectChannelByRoomJid).not.toHaveBeenCalled();
    h.scope.stop();
  });

  test("personal mentions route to the mention notification", async () => {
    const h = makeHarness({ mode: "on-mention" });
    h.pendingNotificationActivities.value = [
      activityEvent({ mentions: ["alice@example.test"] }),
    ];
    await nextTick();

    expect(h.showMentionNotification).toHaveBeenCalledTimes(1);
    expect(h.showChannelMessageNotification).not.toHaveBeenCalled();
    h.scope.stop();
  });

  test("on-mention mode suppresses plain channel messages", async () => {
    const h = makeHarness({ mode: "on-mention" });
    h.pendingNotificationActivities.value = [activityEvent()];
    await nextTick();

    expect(h.showMentionNotification).not.toHaveBeenCalled();
    expect(h.showChannelMessageNotification).not.toHaveBeenCalled();
    // The queue is still drained so stale events never replay.
    expect(h.pendingNotificationActivities.value).toEqual([]);
    h.scope.stop();
  });

  test("presence do-not-disturb silences even broadcast mentions", async () => {
    const h = makeHarness({ doNotDisturb: true });
    h.pendingNotificationActivities.value = [
      activityEvent({ broadcastMention: "everyone" }),
    ];
    await nextTick();

    expect(h.showMentionNotification).not.toHaveBeenCalled();
    expect(h.showChannelMessageNotification).not.toHaveBeenCalled();
    h.scope.stop();
  });
});

describe("useNotificationOrchestration DM activity", () => {
  test("shows a DM notification wired to open the conversation", () => {
    const h = makeHarness();
    h.orchestration.notifyDmActivity(dmMessage());

    expect(h.showDmNotification).toHaveBeenCalledTimes(1);
    const opts = h.showDmNotification.mock.calls[0]?.[0] as unknown as {
      peerJid: string;
      onNavigate?: (peerJid: string) => void;
    };
    expect(opts.peerJid).toBe("bob@example.test");
    opts.onNavigate?.("bob@example.test");
    expect(h.openDm).toHaveBeenCalledWith("bob@example.test");
    h.scope.stop();
  });

  test("suppresses the banner while the user is viewing that DM", () => {
    const h = makeHarness({ sidebarMode: "dms", activePeerJid: "bob@example.test" });
    h.orchestration.notifyDmActivity(dmMessage());

    expect(h.showDmNotification).not.toHaveBeenCalled();
    h.scope.stop();
  });

  test("ignores DM echoes from the user's own account", () => {
    const h = makeHarness();
    h.orchestration.notifyDmActivity(dmMessage({ fromJid: "alice@example.test/web" }));

    expect(h.showDmNotification).not.toHaveBeenCalled();
    h.scope.stop();
  });
});
