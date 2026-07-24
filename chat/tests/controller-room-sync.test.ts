import { describe, expect, mock, test } from "bun:test";
import { computed, effectScope, nextTick, ref } from "vue";
import { useChatShellState } from "../src/shell/state";
import { useRoomSync } from "../src/shell/controllers/use-room-sync";
import type { ExtensionRouteKey } from "../src/shell/controllers/use-extension-routes";
import { handlerStubs } from "./helpers/xmpp-client-mock";
import type { WaddleSession } from "../src/lib/server-auth";
import type { BrowserXmppClient, ChannelSummary } from "../src/lib/xmpp-client";
import type { useWaddleDirectory } from "../src/waddles/directory";
import type { useChannelMessages } from "../src/channels/messages";
import type { useDirectMessageConversations } from "../src/dms/conversations";
import type { useChatReadActivity } from "../src/shell/read-activity";

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

function makeHarness() {
  const ui = useChatShellState();
  const activeChannelId = ref<string | null>(null);
  const currentChannel = ref<ChannelSummary | null>(null);
  const channels = ref<ChannelSummary[]>([]);
  const memberJidByNick = ref<Record<string, string>>({ bob: "bob@example.com" });
  const activeExtensionRouteKey = ref<ExtensionRouteKey | null>({
    channelId: "x",
    pluginId: "p",
    routeId: "r",
  });

  const rememberRoomJidForChannel = mock(() => {});
  const rememberChannelRoomJid = mock(() => {});
  const clearMessages = mock(() => {});
  const clearChannelActivity = mock(() => {});
  const loadMessages = mock(async () => {});
  const reloadChannelMembers = mock(async () => {});
  const closeDm = mock(() => {});
  const updateUrl = mock(() => {});

  const client = {
    ...handlerStubs(),
    rememberRoomJidForChannel,
  } as unknown as BrowserXmppClient;

  const waddles = {
    activeChannelId,
    currentChannel,
    channels,
    groupDms: ref([]),
    reloadChannelMembers,
  } as unknown as ReturnType<typeof useWaddleDirectory>;
  const messaging = {
    rememberChannelRoomJid,
    clearMessages,
    clearChannelActivity,
    loadMessages,
  } as unknown as ReturnType<typeof useChannelMessages>;
  const dmConversations = {
    closeDm,
  } as unknown as ReturnType<typeof useDirectMessageConversations>;
  const computedChannelUnreadMap = computed(() => ({
    general: { unread: 7, mentions: 1 },
  })) as ReturnType<typeof useChatReadActivity>["computedChannelUnreadMap"];

  const scope = effectScope();
  const roomSync = scope.run(() =>
    useRoomSync({
      ui,
      xmppClient: computed(() => client),
      session: computed(() => session()),
      waddles,
      messaging,
      dmConversations,
      computedChannelUnreadMap,
      managedMucDomain: computed(() => "muc.example.com"),
      memberJidByNick,
      activeExtensionRouteKey,
      updateUrl,
    }),
  )!;

  return {
    ui,
    scope,
    roomSync,
    activeChannelId,
    currentChannel,
    channels,
    memberJidByNick,
    activeExtensionRouteKey,
    rememberRoomJidForChannel,
    rememberChannelRoomJid,
    clearMessages,
    clearChannelActivity,
    loadMessages,
    reloadChannelMembers,
    closeDm,
    updateUrl,
  };
}

describe("useRoomSync selectChannel", () => {
  test("records the explicit room JID and loads with the unread count", async () => {
    const h = makeHarness();
    await h.roomSync.selectChannel("general", { roomJid: "general@muc.example.com/resource" });

    // Explicit room JID is normalised to a bare JID everywhere it is recorded.
    expect(h.roomSync.selectedChannelRoomJids.value).toEqual({
      general: "general@muc.example.com",
    });
    expect(h.rememberRoomJidForChannel).toHaveBeenCalledWith("general", "general@muc.example.com");
    expect(h.rememberChannelRoomJid).toHaveBeenCalledWith("general", "general@muc.example.com");
    // Selection bookkeeping: extension panel closed, mention map reset,
    // DM surface closed, mobile drawer dismissed.
    expect(h.activeExtensionRouteKey.value).toBeNull();
    expect(h.memberJidByNick.value).toEqual({});
    expect(h.closeDm).toHaveBeenCalledTimes(1);
    expect(h.ui.activePage.value).toBe("chat");
    expect(h.ui.sidebarMode.value).toBe("channels");
    expect(h.ui.showMobileNav.value).toBe(false);
    // XEP-0502 activity indicator cleared for the selected room.
    expect(h.clearChannelActivity).toHaveBeenCalledWith("general@muc.example.com");
    expect(h.clearMessages).toHaveBeenCalledTimes(1);
    // Unread-at-load comes from the computed unread map.
    expect(h.loadMessages).toHaveBeenCalledWith(
      "",
      "general",
      7,
      [],
      { allowAccessRetry: true },
    );
    expect(h.activeChannelId.value).toBe("general");
    h.scope.stop();
  });

  test("keeps the DM surface when selecting a group-DM room", async () => {
    const h = makeHarness();
    await h.roomSync.selectChannel("group-1", { surface: "dms" });

    expect(h.ui.sidebarMode.value).toBe("dms");
    expect(h.closeDm).not.toHaveBeenCalled();
    h.scope.stop();
  });
});

describe("useRoomSync selectChannelByRoomJid", () => {
  test("parks a trusted managed room until its channel is discovered, then selects it", async () => {
    const h = makeHarness();
    await h.roomSync.selectChannelByRoomJid("general@muc.example.com");

    // Not in the channel list yet: selection is parked, UI flips to the
    // channels surface, but nothing loads.
    expect(h.roomSync.pendingChannelRoomJidSelection.value).toBe("general@muc.example.com");
    expect(h.ui.activePage.value).toBe("chat");
    expect(h.activeExtensionRouteKey.value).toBeNull();
    expect(h.loadMessages).not.toHaveBeenCalled();

    // Structure discovery lands the channel; the watch resolves the
    // parked selection.
    h.channels.value = [{ id: "general", name: "General" } as ChannelSummary];
    await nextTick();
    await nextTick();

    expect(h.roomSync.pendingChannelRoomJidSelection.value).toBeNull();
    expect(h.activeChannelId.value).toBe("general");
    expect(h.loadMessages).toHaveBeenCalledWith(
      "",
      "general",
      7,
      [],
      { allowAccessRetry: true },
    );
    h.scope.stop();
  });

  test("ignores rooms outside the managed MUC domain", async () => {
    const h = makeHarness();
    await h.roomSync.selectChannelByRoomJid("random@rooms.elsewhere.org");

    expect(h.roomSync.pendingChannelRoomJidSelection.value).toBeNull();
    expect(h.ui.activePage.value).toBe("dashboard");
    expect(h.loadMessages).not.toHaveBeenCalled();
    h.scope.stop();
  });

  test("selects immediately when the room maps to a known channel", async () => {
    const h = makeHarness();
    h.channels.value = [{ id: "general", name: "General" } as ChannelSummary];

    await h.roomSync.selectChannelByRoomJid("general@muc.example.com");

    expect(h.roomSync.pendingChannelRoomJidSelection.value).toBeNull();
    expect(h.activeChannelId.value).toBe("general");
    h.scope.stop();
  });
});

describe("useRoomSync activeChannelRoomJid", () => {
  test("prefers the channel's own JID, then the explicit selection", async () => {
    const h = makeHarness();
    h.activeChannelId.value = "general";
    h.currentChannel.value = {
      id: "general",
      name: "General",
      jid: "custom@rooms.example.com/nick",
    } as ChannelSummary;
    expect(h.roomSync.activeChannelRoomJid.value).toBe("custom@rooms.example.com");

    h.currentChannel.value = { id: "general", name: "General" } as ChannelSummary;
    await h.roomSync.selectChannel("general", { roomJid: "general@muc.example.com" });
    expect(h.roomSync.activeChannelRoomJid.value).toBe("general@muc.example.com");
    h.scope.stop();
  });
});
