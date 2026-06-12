import { afterEach, describe, expect, mock, test } from "bun:test";
import { shouldShowChannelForegroundNotification, usePushNotifications } from "../src/shell/notifications";
import {
  showForegroundNotificationForChannelActivity,
  showForegroundNotificationsForChannelActivities,
} from "../src/shell/chat-app-controller";
import type { NotifyMode } from "../src/lib/xmpp-client";

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

function createChannelActivityDispatchHarness(mode: NotifyMode) {
  const onNavigate = mock((_roomJid: string) => {});
  const notifications = {
    showMentionNotification: mock((_opts: unknown) => {}),
    showChannelMessageNotification: mock((_opts: unknown) => {}),
  };
  return {
    onNavigate,
    notifications,
    deps: {
      notifySettings: {
        getMode: mock(() => mode),
      },
      notifications,
      sessionJid: "alice@example.com/web",
      resolveChannelNameFromJid: mock((_roomJid: string) => "General"),
      onNavigate,
    },
  };
}
