import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelUnread } from "../src/composables/useChannelUnread";
import type { BrowserXmppClient, InboxEntry } from "../src/lib/xmpp-client";

type MockClient = BrowserXmppClient & {
  fetchInbox: ReturnType<typeof mock>;
  markInboxRead: ReturnType<typeof mock>;
};

function makeClient(conversations: InboxEntry[] = []): MockClient {
  return {
    fetchInbox: mock(() => Promise.resolve({
      totalUnread: conversations.reduce((sum, conversation) => sum + conversation.unread, 0),
      conversations,
    })),
    markInboxRead: mock(() => Promise.resolve()),
  } as unknown as MockClient;
}

const channelEntry: InboxEntry = {
  partner: "space_channel@conference.example.com",
  kind: "muc",
  lastStanzaId: "sid-1",
  lastUpdated: 100,
  unread: 2,
};

const threadEntry: InboxEntry = {
  partner: "space_channel@conference.example.com",
  kind: "muc",
  lastStanzaId: "sid-2",
  lastUpdated: 200,
  unread: 3,
  thread: "thread-1",
  threadTitle: "Planning",
};

describe("useChannelUnread", () => {
  test("hydrates channel and thread unread totals", async () => {
    const client = makeClient([
      channelEntry,
      threadEntry,
      {
        partner: "bob@example.com",
        kind: "direct",
        lastStanzaId: "sid-3",
        lastUpdated: 300,
        unread: 7,
      },
    ]);
    const composable = useChannelUnread(ref<BrowserXmppClient | null>(client));

    await composable.hydrateFromInbox();

    expect(composable.totalChannelUnreadCount.value).toBe(2);
    expect(composable.totalThreadUnreadCount.value).toBe(3);
    expect(composable.totalUnreadCount.value).toBe(2);
    expect(composable.channelUnreadMap()).toMatchObject({
      space_channel: { unread: 2, mentions: 0 },
    });
  });

  test("updates totals from inbox pushes and ignores direct entries", () => {
    const composable = useChannelUnread(ref<BrowserXmppClient | null>(null));

    composable.onInboxPush(channelEntry);
    composable.onInboxPush(threadEntry);
    composable.onInboxPush({
      partner: "bob@example.com",
      kind: "direct",
      lastStanzaId: "sid-3",
      lastUpdated: 300,
      unread: 7,
    });

    expect(composable.totalChannelUnreadCount.value).toBe(2);
    expect(composable.totalThreadUnreadCount.value).toBe(3);
    expect(composable.totalUnreadCount.value).toBe(2);
  });

  test("markRead and markThreadRead clear their own totals", () => {
    const client = makeClient();
    const composable = useChannelUnread(ref<BrowserXmppClient | null>(client));
    composable.onInboxPush(channelEntry);
    composable.onInboxPush(threadEntry);

    composable.markRead("space_channel@conference.example.com");

    expect(composable.totalChannelUnreadCount.value).toBe(0);
    expect(composable.totalThreadUnreadCount.value).toBe(3);
    expect(composable.totalUnreadCount.value).toBe(0);
    expect(client.markInboxRead).toHaveBeenCalledWith("space_channel@conference.example.com");

    composable.markThreadRead("space_channel@conference.example.com", "thread-1");

    expect(composable.totalThreadUnreadCount.value).toBe(0);
    expect(composable.totalUnreadCount.value).toBe(0);
    expect(client.markInboxRead).toHaveBeenCalledWith("space_channel@conference.example.com", "thread-1");
  });

  test("clears stale unread state without a client", async () => {
    const client = ref<BrowserXmppClient | null>(makeClient());
    const composable = useChannelUnread(client);
    composable.onInboxPush(channelEntry);
    composable.onInboxPush(threadEntry);

    client.value = null;
    const hydrated = await composable.hydrateFromInbox();

    expect(hydrated).toBe(false);
    expect(composable.totalChannelUnreadCount.value).toBe(0);
    expect(composable.totalThreadUnreadCount.value).toBe(0);
    expect(composable.totalUnreadCount.value).toBe(0);
  });
});
