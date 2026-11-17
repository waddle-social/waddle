import { describe, expect, mock, test } from "bun:test";
import { effectScope, ref } from "vue";
import { applyEntries, createInboxState, markReadInState } from "../src/services/inbox";
import { useChatReadActivity } from "../src/shell/read-activity";

function makeReadActivityHarness(options: {
  unread: number;
  latestId: string | null;
  sidebarMode?: "channels" | "dms";
  activePeerJid?: string | null;
  emptyConversation?: boolean;
}) {
  const roomJid = "general@conference.example.com";
  const unread = ref(options.unread);
  const groupJid = "group@conference.example.com";
  const inboxState = ref(createInboxState());
  const latestRemoteMessageId = ref<string | null>(options.latestId);
  const markRead = mock(() => undefined);
  const clearChannelActivity = mock(() => undefined);
  const markDisplayed = mock(() => undefined);
  const scope = effectScope();

  const activity = scope.run(() =>
    useChatReadActivity({
      appReady: ref(true),
      session: ref({} as never),
      xmppClient: ref({} as never),
      activePage: ref("chat"),
      sidebarMode: ref(options.sidebarMode ?? "channels"),
      activeChannelId: ref("general"),
      channels: ref([
        { id: "general", name: "General", jid: roomJid },
        { id: "group", name: "Group", jid: groupJid, isGroupDm: true },
      ]),
      channelUnread: {
        inboxState,
        totalUnreadCount: ref(0),
        channelUnreadMap: () => ({}),
        hydrateFromInbox: mock(async () => true),
        markRead,
        unreadForRoomJid: (jid: string) => jid === roomJid ? unread.value : 0,
      } as never,
      dmConversations: {
        totalUnreadCount: ref(0),
        conversations: ref([]),
        activePeerJid: ref(options.activePeerJid ?? null),
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
      activeTarget: ref(options.emptyConversation ? null : { markDisplayed }),
      roomJidForChannelId: () => roomJid,
    }),
  )!;

  return {
    roomJid,
    groupJid,
    inboxState,
    inboxUnreadCount: activity.inboxUnreadCount,
    clearChannelActivity,
    markDisplayed,
    markRead,
    stop: () => scope.stop(),
  };
}

describe("useChatReadActivity", () => {
  test("counts unread threads in Inbox rooms and excludes group DMs and unknown rooms", () => {
    const h = makeReadActivityHarness({ unread: 0, latestId: null });
    const entry = { kind: "muc" as const, lastStanzaId: "s", lastUpdated: 1 };
    try {
      h.inboxState.value = applyEntries(h.inboxState.value, [
        { ...entry, partner: h.roomJid, thread: "t1", unread: 2 },
        { ...entry, partner: h.groupJid, unread: 4 },
        { ...entry, partner: "general@old.example.com", unread: 3 },
      ]);
      expect(h.inboxUnreadCount.value).toBe(2);

      h.inboxState.value = markReadInState(h.inboxState.value, h.roomJid, "t1");
      expect(h.inboxUnreadCount.value).toBe(0);
    } finally {
      h.stop();
    }
  });

  test("an empty DM view does not mark the previous channel as read", async () => {
    const h = makeReadActivityHarness({
      unread: 2, latestId: "remote-1", sidebarMode: "dms", emptyConversation: true,
    });
    try {
      await Promise.resolve();
      expect(h.markDisplayed).not.toHaveBeenCalled();
      expect(h.markRead).not.toHaveBeenCalled();
      expect(h.clearChannelActivity).not.toHaveBeenCalled();
    } finally {
      h.stop();
    }
  });

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

  test("treats a DM-sidebar group DM as the active channel/MUC read target", async () => {
    const h = makeReadActivityHarness({ unread: 1, latestId: "remote-1", sidebarMode: "dms" });

    try {
      await Promise.resolve();

      expect(h.markDisplayed).toHaveBeenCalledWith("remote-1");
      expect(h.markRead).toHaveBeenCalledWith(h.roomJid);
      expect(h.clearChannelActivity).toHaveBeenCalledWith(h.roomJid);
    } finally {
      h.stop();
    }
  });
});
