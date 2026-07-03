import { describe, expect, mock, test } from "bun:test";
import { computed, effectScope, nextTick, ref } from "vue";
import { useChatShellState } from "../src/shell/state";
import { useNotificationOrchestration } from "../src/shell/controllers/use-notification-orchestration";
import type { LiveDmMessage, NotifyMode, RoomActivityEvent } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { NotifySettingsStore } from "../src/lib/notify-settings";
import type { usePushNotifications } from "../src/shell/notifications";
import type { createBrowserMessageTonePlayer } from "../src/shell/audio-alerts";
import type { useWaddleDirectory } from "../src/waddles/directory";
import type { useDirectMessageConversations } from "../src/dms/conversations";

function session(): WaddleSession {
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
  } as WaddleSession;
}

function activityEvent(partial: Partial<RoomActivityEvent> = {}): RoomActivityEvent {
  return {
    roomJid: "general@muc.example.com",
    nick: "bob",
    body: "hello",
    stanzaId: "st-1",
    ...partial,
  };
}

function dmMessage(partial: Partial<LiveDmMessage> = {}): LiveDmMessage {
  return {
    id: "m1",
    peerJid: "bob@example.com",
    fromJid: "bob@example.com/phone",
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
} = {}) {
  const ui = useChatShellState();
  ui.sidebarMode.value = options.sidebarMode ?? "channels";
  const pendingNotificationActivities = ref<RoomActivityEvent[]>([]);
  const showMentionNotification = mock(() => {});
  const showChannelMessageNotification = mock(() => {});
  const showDmNotification = mock(() => {});
  const selectChannel = mock(async () => {});
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
    channels: ref([{ id: "general", name: "General" }]),
  } as unknown as ReturnType<typeof useWaddleDirectory>;

  const dmConversations = {
    activePeerJid: ref<string | null>(options.activePeerJid ?? null),
  } as unknown as ReturnType<typeof useDirectMessageConversations>;

  const scope = effectScope();
  const orchestration = scope.run(() =>
    useNotificationOrchestration({
      ui,
      xmppClient: computed(() => null),
      session: computed(() => session()),
      waddles,
      dmConversations,
      notifications,
      notifySettings,
      messageSound,
      isWindowFocused: ref(true),
      isSelfDoNotDisturb: () => options.doNotDisturb ?? false,
      pendingNotificationActivities,
      selectChannel,
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
    selectChannel,
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

    // Navigation resolves the managed room JID back to its channel id.
    opts.onNavigate?.("general@muc.example.com");
    expect(h.selectChannel).toHaveBeenCalledWith("general");
    h.scope.stop();
  });

  test("personal mentions route to the mention notification", async () => {
    const h = makeHarness({ mode: "on-mention" });
    h.pendingNotificationActivities.value = [
      activityEvent({ mentions: ["alice@example.com"] }),
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
    expect(opts.peerJid).toBe("bob@example.com");
    opts.onNavigate?.("bob@example.com");
    expect(h.openDm).toHaveBeenCalledWith("bob@example.com");
    h.scope.stop();
  });

  test("suppresses the banner while the user is viewing that DM", () => {
    const h = makeHarness({ sidebarMode: "dms", activePeerJid: "bob@example.com" });
    h.orchestration.notifyDmActivity(dmMessage());

    expect(h.showDmNotification).not.toHaveBeenCalled();
    h.scope.stop();
  });

  test("ignores DM echoes from the user's own account", () => {
    const h = makeHarness();
    h.orchestration.notifyDmActivity(dmMessage({ fromJid: "alice@example.com/web" }));

    expect(h.showDmNotification).not.toHaveBeenCalled();
    h.scope.stop();
  });
});
