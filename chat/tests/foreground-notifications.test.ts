import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  shouldShowChannelForegroundNotification,
  usePushNotifications,
} from "../src/shell/notifications";
import { createIncomingCallAlertController } from "../src/shell/audio-alerts";
import {
  showForegroundNotificationForDmActivity,
  showForegroundNotificationForChannelActivity,
  showForegroundNotificationsForChannelActivities,
} from "../src/shell/chat-app-controller";
import type { LiveDmMessage, NotifyMode } from "../src/lib/xmpp-client";

const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const originalNotification = Object.getOwnPropertyDescriptor(globalThis, "Notification");
const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");

afterEach(() => {
  restoreGlobal("window", originalWindow);
  restoreGlobal("Notification", originalNotification);
  restoreGlobal("navigator", originalNavigator);
});

describe("channel foreground notification matrix", () => {
  const cases: Array<{
    mode: NotifyMode;
    isMention: boolean;
    expected: boolean;
  }> = [
    { mode: "always", isMention: false, expected: true },
    { mode: "always", isMention: true, expected: true },
    { mode: "on-mention", isMention: false, expected: false },
    { mode: "on-mention", isMention: true, expected: true },
    { mode: "never", isMention: false, expected: false },
    { mode: "never", isMention: true, expected: false },
  ];

  for (const { mode, isMention, expected } of cases) {
    test(`${mode} ${isMention ? "shows" : "handles"} ${isMention ? "mention" : "plain"} messages`, () => {
      expect(shouldShowChannelForegroundNotification({ mode, isMention })).toBe(expected);
    });
  }
});

test("channel foreground notification titles include sender and conversation and signal SW dedup", () => {
  const notifications = installNotificationHarness();
  const postMessage = mock((_message: unknown) => {});
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: {
      serviceWorker: {
        controller: { postMessage },
      },
    },
  });

  const pushNotifications = usePushNotifications();
  pushNotifications.showChannelMessageNotification({
    senderNick: "bob",
    channelName: "General",
    body: "plain update",
    roomJid: "general@conference.example.com",
    stanzaId: "stanza-plain-1",
  });

  expect(notifications).toEqual([
    {
      title: "@bob in #General",
      options: {
        body: "plain update",
        tag: "general@conference.example.com",
        icon: "/android-chrome-192x192.png",
      },
    },
  ]);
  expect(postMessage).toHaveBeenCalledWith({
    type: "waddle:item-shown",
    itemId: "stanza-plain-1",
  });
});

test("message sound toggle persists and silences only message sound playback", async () => {
  const localStorage = memoryStorage();
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { localStorage },
  });
  const first = usePushNotifications();
  expect(first.messageSoundsEnabled.value).toBe(true);

  first.messageSoundsEnabled.value = false;
  await Promise.resolve();
  expect(localStorage.getItem("waddle.chat.message-sounds-enabled")).toBe("false");

  const reloaded = usePushNotifications();
  expect(reloaded.messageSoundsEnabled.value).toBe(false);

  const harness = createChannelActivityDispatchHarness("always", {
    focused: false,
    messageSoundsEnabled: reloaded.messageSoundsEnabled.value,
  });
  showForegroundNotificationForChannelActivity({
    roomJid: "general@conference.example.com",
    nick: "bob",
    body: "plain update",
    stanzaId: "stanza-muted-globally",
  }, harness.deps);

  expect(harness.notifications.showChannelMessageNotification).toHaveBeenCalled();
  expect(harness.messageSound.play).not.toHaveBeenCalled();
});

test("disabled message sounds do not affect incoming-call ringing", async () => {
  const localStorage = memoryStorage();
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { localStorage },
  });
  const pushNotifications = usePushNotifications();
  pushNotifications.messageSoundsEnabled.value = false;
  await Promise.resolve();

  const player = {
    startLoop: mock((_key: string) => {}),
    stop: mock((_key: string) => {}),
  };
  const controller = createIncomingCallAlertController({ player });

  controller.start({
    peerJid: "bob@example.com",
    sid: "call-1",
    media: { audio: true, video: false },
  });

  expect(player.startLoop).toHaveBeenCalledWith("call-1");
});

describe("channel activity foreground notification dispatch", () => {
  test("always-mode plain activity uses the channel-message foreground renderer", () => {
    const harness = createChannelActivityDispatchHarness("always");

    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "plain update",
      stanzaId: "stanza-plain",
    }, harness.deps);

    expect(harness.notifications.showChannelMessageNotification).toHaveBeenCalledWith({
      senderNick: "bob",
      channelName: "General",
      body: "plain update",
      roomJid: "general@conference.example.com",
      stanzaId: "stanza-plain",
      onNavigate: harness.onNavigate,
    });
    expect(harness.notifications.showMentionNotification).not.toHaveBeenCalled();
  });

  test("always-mode plain activity plays the message sound only while unfocused", () => {
    const unfocused = createChannelActivityDispatchHarness("always", { focused: false });

    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "plain update",
      stanzaId: "stanza-plain",
    }, unfocused.deps);

    expect(unfocused.messageSound.play).toHaveBeenCalledWith("message:general@conference.example.com:stanza-plain");

    const focused = createChannelActivityDispatchHarness("always", { focused: true });
    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "plain update",
      stanzaId: "stanza-focused",
    }, focused.deps);

    expect(focused.messageSound.play).not.toHaveBeenCalled();
  });

  test("on-mention suppresses plain activity and routes mentions through the mention renderer", () => {
    const plain = createChannelActivityDispatchHarness("on-mention");
    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "plain update",
    }, plain.deps);
    expect(plain.notifications.showChannelMessageNotification).not.toHaveBeenCalled();
    expect(plain.notifications.showMentionNotification).not.toHaveBeenCalled();

    const mention = createChannelActivityDispatchHarness("on-mention");
    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "alice please review",
      mentions: ["xmpp:alice@example.com"],
      stanzaId: "stanza-mention",
    }, mention.deps);
    expect(mention.notifications.showMentionNotification).toHaveBeenCalledWith({
      senderNick: "bob",
      channelName: "General",
      body: "alice please review",
      roomJid: "general@conference.example.com",
      isBroadcast: false,
      stanzaId: "stanza-mention",
      onNavigate: mention.onNavigate,
    });
    expect(mention.notifications.showChannelMessageNotification).not.toHaveBeenCalled();
  });

  test("group DM notification modes use the private-group display matrix", () => {
    const plain = createChannelActivityDispatchHarness("always", { channelName: "Launch Crew" });
    showForegroundNotificationForChannelActivity({
      roomJid: "group-dm-launch@muc.example.com",
      nick: "bob",
      body: "plain group DM update",
      stanzaId: "group-dm-plain",
    }, plain.deps);
    expect(plain.notifications.showChannelMessageNotification).toHaveBeenCalledWith({
      senderNick: "bob",
      channelName: "Launch Crew",
      body: "plain group DM update",
      roomJid: "group-dm-launch@muc.example.com",
      stanzaId: "group-dm-plain",
      onNavigate: plain.onNavigate,
    });

    const mentionsOnlyPlain = createChannelActivityDispatchHarness("on-mention", { channelName: "Launch Crew" });
    showForegroundNotificationForChannelActivity({
      roomJid: "group-dm-launch@muc.example.com",
      nick: "bob",
      body: "plain group DM update",
      stanzaId: "group-dm-mentions-only-plain",
    }, mentionsOnlyPlain.deps);
    expect(mentionsOnlyPlain.notifications.showChannelMessageNotification).not.toHaveBeenCalled();
    expect(mentionsOnlyPlain.notifications.showMentionNotification).not.toHaveBeenCalled();

    const mentionsOnlyMention = createChannelActivityDispatchHarness("on-mention", { channelName: "Launch Crew" });
    showForegroundNotificationForChannelActivity({
      roomJid: "group-dm-launch@muc.example.com",
      nick: "bob",
      body: "alice please review",
      mentions: ["xmpp:alice@example.com"],
      stanzaId: "group-dm-mentions-only-hit",
    }, mentionsOnlyMention.deps);
    expect(mentionsOnlyMention.notifications.showMentionNotification).toHaveBeenCalledWith({
      senderNick: "bob",
      channelName: "Launch Crew",
      body: "alice please review",
      roomJid: "group-dm-launch@muc.example.com",
      isBroadcast: false,
      stanzaId: "group-dm-mentions-only-hit",
      onNavigate: mentionsOnlyMention.onNavigate,
    });
    expect(mentionsOnlyMention.notifications.showChannelMessageNotification).not.toHaveBeenCalled();

    const never = createChannelActivityDispatchHarness("never", { channelName: "Launch Crew" });
    showForegroundNotificationForChannelActivity({
      roomJid: "group-dm-launch@muc.example.com",
      nick: "bob",
      body: "alice please review",
      mentions: ["xmpp:alice@example.com"],
      stanzaId: "group-dm-never",
    }, never.deps);
    expect(never.notifications.showChannelMessageNotification).not.toHaveBeenCalled();
    expect(never.notifications.showMentionNotification).not.toHaveBeenCalled();
  });

  test("message sounds follow the channel foreground notification eligibility matrix", () => {
    const plainMentionOnly = createChannelActivityDispatchHarness("on-mention", { focused: false });
    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "plain update",
      stanzaId: "stanza-plain",
    }, plainMentionOnly.deps);
    expect(plainMentionOnly.messageSound.play).not.toHaveBeenCalled();

    const mentioned = createChannelActivityDispatchHarness("on-mention", { focused: false });
    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "alice please review",
      mentions: ["xmpp:alice@example.com"],
      stanzaId: "stanza-mention",
    }, mentioned.deps);
    expect(mentioned.messageSound.play).toHaveBeenCalledWith("message:general@conference.example.com:stanza-mention");

    const never = createChannelActivityDispatchHarness("never", { focused: false });
    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "@everyone update",
      broadcastMention: "everyone",
      stanzaId: "stanza-never",
    }, never.deps);
    expect(never.messageSound.play).not.toHaveBeenCalled();
  });

  test("message sounds stop when foreground notifications or DND suppress the channel notification", () => {
    const notificationsOff = createChannelActivityDispatchHarness("always", {
      focused: false,
      canShowForegroundNotification: false,
    });
    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "plain update",
      stanzaId: "stanza-notifications-off",
    }, notificationsOff.deps);
    expect(notificationsOff.notifications.showChannelMessageNotification).not.toHaveBeenCalled();
    expect(notificationsOff.messageSound.play).not.toHaveBeenCalled();

    const dnd = createChannelActivityDispatchHarness("always", {
      focused: false,
      doNotDisturb: true,
    });
    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "plain update",
      stanzaId: "stanza-dnd",
    }, dnd.deps);
    expect(dnd.notifications.showChannelMessageNotification).not.toHaveBeenCalled();
    expect(dnd.messageSound.play).not.toHaveBeenCalled();
  });

  test("unstamped duplicate channel messages each get their own message sound key", () => {
    const harness = createChannelActivityDispatchHarness("always", { focused: false });
    const event = {
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "ok",
    };

    showForegroundNotificationForChannelActivity(event, harness.deps);
    showForegroundNotificationForChannelActivity(event, harness.deps);

    expect(harness.messageSound.play).toHaveBeenCalledTimes(2);
    expect(harness.messageSound.play.mock.calls[0]?.[0]).not.toBe(
      harness.messageSound.play.mock.calls[1]?.[0],
    );
  });

  test("never suppresses both plain and mention activity", () => {
    const harness = createChannelActivityDispatchHarness("never");

    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "plain update",
    }, harness.deps);
    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "@everyone update",
      broadcastMention: "everyone",
    }, harness.deps);

    expect(harness.notifications.showChannelMessageNotification).not.toHaveBeenCalled();
    expect(harness.notifications.showMentionNotification).not.toHaveBeenCalled();
  });

  test("allowed broadcast mentions route through the mention renderer as broadcast", () => {
    const harness = createChannelActivityDispatchHarness("on-mention");

    showForegroundNotificationForChannelActivity({
      roomJid: "general@conference.example.com",
      nick: "bob",
      body: "@everyone deploy starting",
      broadcastMention: "everyone",
      stanzaId: "stanza-broadcast",
    }, harness.deps);

    expect(harness.notifications.showMentionNotification).toHaveBeenCalledWith({
      senderNick: "bob",
      channelName: "General",
      body: "@everyone deploy starting",
      roomJid: "general@conference.example.com",
      isBroadcast: true,
      stanzaId: "stanza-broadcast",
      onNavigate: harness.onNavigate,
    });
    expect(harness.notifications.showChannelMessageNotification).not.toHaveBeenCalled();
  });

  test("queued activity drain renders every foreground-eligible event", () => {
    const harness = createChannelActivityDispatchHarness("always");

    showForegroundNotificationsForChannelActivities([
      {
        roomJid: "general@conference.example.com",
        nick: "bob",
        body: "first",
        stanzaId: "stanza-first",
      },
      {
        roomJid: "general@conference.example.com",
        nick: "carol",
        body: "second",
        stanzaId: "stanza-second",
      },
    ], harness.deps);

    expect(harness.notifications.showChannelMessageNotification).toHaveBeenCalledTimes(2);
    expect(harness.notifications.showChannelMessageNotification.mock.calls.map(([opts]) => (
      (opts as { stanzaId?: string }).stanzaId
    ))).toEqual(["stanza-first", "stanza-second"]);
  });
});

describe("DM foreground notification dispatch", () => {
  test("unfocused always-mode DM plays a message sound", () => {
    const harness = createDmActivityDispatchHarness("always", { focused: false });

    showForegroundNotificationForDmActivity(dmMessage(), harness.deps);

    expect(harness.notifications.showDmNotification).toHaveBeenCalledWith({
      senderUsername: "bob",
      peerJid: "bob@example.com",
      body: "hello",
      stanzaId: "dm-stanza-1",
      onNavigate: harness.onNavigate,
    });
    expect(harness.messageSound.play).toHaveBeenCalledWith("message:bob@example.com:dm-stanza-1");
  });

  test("focused inactive DM shows notification without message sound", () => {
    const harness = createDmActivityDispatchHarness("always", { focused: true });

    showForegroundNotificationForDmActivity(dmMessage(), harness.deps);

    expect(harness.notifications.showDmNotification).toHaveBeenCalledWith({
      senderUsername: "bob",
      peerJid: "bob@example.com",
      body: "hello",
      stanzaId: "dm-stanza-1",
      onNavigate: expect.any(Function),
    });
    expect(harness.messageSound.play).not.toHaveBeenCalled();
  });

  test("never-mode DM suppresses notifications and message sounds", () => {
    const harness = createDmActivityDispatchHarness("never", { focused: false });

    showForegroundNotificationForDmActivity(dmMessage(), harness.deps);

    expect(harness.notifications.showDmNotification).not.toHaveBeenCalled();
    expect(harness.messageSound.play).not.toHaveBeenCalled();
  });

  test("self-sent and currently open DMs suppress notifications and message sounds", () => {
    const self = createDmActivityDispatchHarness("always", { focused: false });
    showForegroundNotificationForDmActivity(dmMessage({ fromJid: "alice@example.com/web" }), self.deps);
    expect(self.notifications.showDmNotification).not.toHaveBeenCalled();
    expect(self.messageSound.play).not.toHaveBeenCalled();

    const open = createDmActivityDispatchHarness("always", {
      focused: false,
      activePeerJid: "bob@example.com",
    });
    showForegroundNotificationForDmActivity(dmMessage(), open.deps);
    expect(open.notifications.showDmNotification).not.toHaveBeenCalled();
    expect(open.messageSound.play).not.toHaveBeenCalled();
  });

  test("foreground notification disablement and DND suppress DM message sounds", () => {
    const notificationsOff = createDmActivityDispatchHarness("always", {
      focused: false,
      canShowForegroundNotification: false,
    });
    showForegroundNotificationForDmActivity(dmMessage(), notificationsOff.deps);
    expect(notificationsOff.notifications.showDmNotification).not.toHaveBeenCalled();
    expect(notificationsOff.messageSound.play).not.toHaveBeenCalled();

    const dnd = createDmActivityDispatchHarness("always", {
      focused: false,
      doNotDisturb: true,
    });
    showForegroundNotificationForDmActivity(dmMessage(), dnd.deps);
    expect(dnd.notifications.showDmNotification).not.toHaveBeenCalled();
    expect(dnd.messageSound.play).not.toHaveBeenCalled();
  });
});

function installNotificationHarness(): Array<{ title: string; options?: NotificationOptions }> {
  const notifications: Array<{ title: string; options?: NotificationOptions }> = [];
  const localStorage = memoryStorage();
  localStorage.setItem("waddle.chat.notifications-enabled", "true");

  class FakeNotification {
    static permission: NotificationPermission = "granted";
    static requestPermission = mock(async () => "granted" as NotificationPermission);
    onclick: (() => void) | null = null;

    constructor(title: string, options?: NotificationOptions) {
      notifications.push({ title, options });
    }

    close() {}
  }

  Object.defineProperty(globalThis, "Notification", {
    configurable: true,
    value: FakeNotification,
  });
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      Notification: FakeNotification,
      localStorage,
      focus: mock(() => {}),
    },
  });
  return notifications;
}

function memoryStorage(): Storage {
  const store = new Map<string, string>();
  return {
    get length() { return store.size; },
    clear: () => store.clear(),
    getItem: (key) => store.get(key) ?? null,
    key: (index) => Array.from(store.keys())[index] ?? null,
    removeItem: (key) => { store.delete(key); },
    setItem: (key, value) => { store.set(key, String(value)); },
  };
}

function restoreGlobal(name: "window" | "Notification" | "navigator", descriptor: PropertyDescriptor | undefined) {
  if (descriptor) {
    Object.defineProperty(globalThis, name, descriptor);
  } else {
    delete (globalThis as Record<string, unknown>)[name];
  }
}

function createChannelActivityDispatchHarness(
  mode: NotifyMode,
  options: {
    focused?: boolean;
    messageSoundsEnabled?: boolean;
    canShowForegroundNotification?: boolean;
    doNotDisturb?: boolean;
    channelName?: string;
  } = {},
) {
  const onNavigate = mock((_roomJid: string) => {});
  const notifications = {
    showMentionNotification: mock((_opts: unknown) => {}),
    showChannelMessageNotification: mock((_opts: unknown) => {}),
  };
  const messageSound = {
    play: mock((_key: string) => {}),
  };
  return {
    onNavigate,
    notifications,
    messageSound,
    deps: {
      notifySettings: {
        getMode: mock(() => mode),
      },
      notifications,
      messageSound,
      messageSoundsEnabled: () => options.messageSoundsEnabled ?? true,
      canShowForegroundNotification: () => options.canShowForegroundNotification ?? true,
      isDoNotDisturb: () => options.doNotDisturb ?? false,
      isTabFocused: () => options.focused ?? false,
      sessionJid: "alice@example.com/web",
      resolveChannelNameFromJid: mock((_roomJid: string) => options.channelName ?? "General"),
      onNavigate,
    },
  };
}

function createDmActivityDispatchHarness(
  mode: NotifyMode,
  options: {
    focused?: boolean;
    messageSoundsEnabled?: boolean;
    canShowForegroundNotification?: boolean;
    doNotDisturb?: boolean;
    activePeerJid?: string | null;
  } = {},
) {
  const onNavigate = mock((_peerJid: string) => {});
  const notifications = {
    showDmNotification: mock((_opts: unknown) => {}),
  };
  const messageSound = {
    play: mock((_key: string) => {}),
  };
  return {
    onNavigate,
    notifications,
    messageSound,
    deps: {
      notifySettings: {
        getMode: mock(() => mode),
      },
      notifications,
      messageSound,
      messageSoundsEnabled: () => options.messageSoundsEnabled ?? true,
      canShowForegroundNotification: () => options.canShowForegroundNotification ?? true,
      isDoNotDisturb: () => options.doNotDisturb ?? false,
      isTabFocused: () => options.focused ?? false,
      sessionJid: "alice@example.com/web",
      activePeerJid: options.activePeerJid ?? null,
      onNavigate,
    },
  };
}

function dmMessage(overrides: Partial<LiveDmMessage> = {}): LiveDmMessage {
  return {
    id: "dm-1",
    peerJid: "bob@example.com",
    fromJid: "bob@example.com/phone",
    nick: "bob",
    body: "hello",
    createdAt: "2026-06-12T10:00:00.000Z",
    createdAtSource: "server",
    type: "message",
    stanzaId: "dm-stanza-1",
    ...overrides,
  };
}
