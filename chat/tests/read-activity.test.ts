import { describe, expect, mock, test } from "bun:test";
import { effectScope, ref } from "vue";
import { useChatReadActivity } from "../src/shell/read-activity";

function makeReadActivityHarness(options: { unread: number; latestId: string | null }) {
  const roomJid = "general@conference.example.com";
  const unread = ref(options.unread);
  const latestRemoteMessageId = ref<string | null>(options.latestId);
  const markRead = mock(() => undefined);
  const clearChannelActivity = mock(() => undefined);
  const markDisplayed = mock(() => undefined);
  const scope = effectScope();

  scope.run(() => {
    useChatReadActivity({
      appReady: ref(true),
      session: ref({} as never),
      xmppClient: ref({} as never),
      activePage: ref("chat"),
      sidebarMode: ref("channels"),
      activeChannelId: ref("general"),
      channels: ref([{ id: "general", name: "General", jid: roomJid }]),
      channelUnread: {
        totalUnreadCount: ref(0),
        channelUnreadMap: () => ({}),
        hydrateFromInbox: mock(async () => true),
        markRead,
        unreadForRoomJid: (jid: string) => jid === roomJid ? unread.value : 0,
      } as never,
      dmConversations: {
        totalUnreadCount: ref(0),
        conversations: ref([]),
        activePeerJid: ref(null),
        hydrateFromInbox: mock(async () => true),
        markRead: mock(() => undefined),
      } as never,
      messaging: {
        isPinnedAtEdge: ref(true),
        latestRemoteMessageId,
        clearChannelActivity,
      } as never,
      dmMessaging: {
        isPinnedAtEdge: ref(true),
        latestRemoteMessageId: ref(null),
      } as never,
      activeTarget: ref({ markDisplayed }),
      roomJidForChannelId: () => roomJid,
    });
  });

  return {
    roomJid,
    clearChannelActivity,
    markDisplayed,
    markRead,
    stop: () => scope.stop(),
  };
}

describe("useChatReadActivity", () => {
  test("clears live channel activity when the active room is marked read", async () => {
    const h = makeReadActivityHarness({ unread: 2, latestId: "remote-1" });

    try {
      await Promise.resolve();

      expect(h.markDisplayed).toHaveBeenCalledWith("remote-1");
      expect(h.markRead).toHaveBeenCalledWith(h.roomJid);
      expect(h.clearChannelActivity).toHaveBeenCalledTimes(1);
      expect(h.clearChannelActivity).toHaveBeenCalledWith(h.roomJid);
    } finally {
      h.stop();
    }
  });

  test("clears live channel activity when a displayed marker advances without inbox unread", async () => {
    const h = makeReadActivityHarness({ unread: 0, latestId: "remote-1" });

    try {
      await Promise.resolve();

      expect(h.markDisplayed).toHaveBeenCalledWith("remote-1");
      expect(h.markRead).not.toHaveBeenCalled();
      expect(h.clearChannelActivity).toHaveBeenCalledTimes(1);
      expect(h.clearChannelActivity).toHaveBeenCalledWith(h.roomJid);
    } finally {
      h.stop();
    }
  });
});
