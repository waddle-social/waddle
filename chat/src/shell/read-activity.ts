import { computed, type Ref } from "vue";
import type { ChannelSummary } from "@/lib/chat-types";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import { useChatReadReceipts } from "@/shell/read-receipts";
import { useChatTabUnreadIndicator } from "@/shell/tab-unread-indicator";
import { useChatWindowVisibility } from "@/shell/window-visibility";

type ReadonlyRef<T> = Readonly<Ref<T>>;
type ActiveTarget = {
  markDisplayed: (messageId: string) => void;
};

export function useChatReadActivity(options: {
  appReady: ReadonlyRef<boolean>;
  session: ReadonlyRef<WaddleSession | null>;
  xmppClient: ReadonlyRef<BrowserXmppClient | null>;
  activePage: ReadonlyRef<string>;
  sidebarMode: ReadonlyRef<"channels" | "dms">;
  activeChannelId: ReadonlyRef<string | null>;
  channels: ReadonlyRef<ChannelSummary[]>;
  channelUnread: ReturnType<typeof import("@/channels/inbox").useChannelInbox>;
  dmConversations: ReturnType<typeof import("@/dms/conversations").useDirectMessageConversations>;
  messaging: ReturnType<typeof import("@/channels/messages").useChannelMessages>;
  dmMessaging: ReturnType<typeof import("@/dms/messages").useDirectMessages>;
  activeTarget: ReadonlyRef<ActiveTarget>;
  roomJidForChannelId: (session: WaddleSession, channels: ChannelSummary[], channelId: string) => string | null;
}) {
  const computedChannelUnreadMap = computed(() => options.channelUnread.channelUnreadMap(options.channels.value));
  const totalTabUnreadCount = computed(() =>
    options.appReady.value && options.session.value && options.xmppClient.value
      ? options.dmConversations.totalUnreadCount.value + options.channelUnread.totalUnreadCount.value
      : 0
  );

  useChatTabUnreadIndicator(totalTabUnreadCount, {
    shouldAcceptServiceWorkerUnreadCount: () =>
      options.appReady.value && !!options.session.value && !!options.xmppClient.value,
    onServiceWorkerUnreadCount: () => Promise.all([
      options.dmConversations.hydrateFromInbox(),
      options.channelUnread.hydrateFromInbox(),
    ]).then((results) => results.every(Boolean)),
  });

  const { isWindowFocused } = useChatWindowVisibility();
  const readReceiptsKind = computed<"channel" | "dm" | null>(() => {
    if (options.activePage.value !== "chat") return null;
    if (options.sidebarMode.value === "dms") {
      return options.dmConversations.activePeerJid.value ? "dm" : null;
    }
    return options.activeChannelId.value ? "channel" : null;
  });
  const readReceiptsActiveRoomJid = computed<string | null>(() => {
    if (readReceiptsKind.value !== "channel") return null;
    const channelId = options.activeChannelId.value;
    const session = options.session.value;
    if (!channelId || !session) return null;
    return options.roomJidForChannelId(session, options.channels.value, channelId);
  });
  const readReceiptsActivePeerJid = computed<string | null>(() =>
    readReceiptsKind.value === "dm" ? options.dmConversations.activePeerJid.value : null,
  );
  const readReceiptsIsPinnedAtEdge = computed<boolean>(() =>
    readReceiptsKind.value === "dm"
      ? options.dmMessaging.isPinnedAtEdge.value
      : options.messaging.isPinnedAtEdge.value,
  );
  const readReceiptsLatestRemoteId = computed<string | null>(() =>
    readReceiptsKind.value === "dm"
      ? options.dmMessaging.latestRemoteMessageId.value
      : options.messaging.latestRemoteMessageId.value,
  );
  const readReceiptsUnreadCount = computed<number>(() => {
    if (readReceiptsKind.value === "dm") {
      const peer = readReceiptsActivePeerJid.value;
      if (!peer) return 0;
      return options.dmConversations.conversations.value.find((c) => c.peerJid === peer)?.unreadCount ?? 0;
    }
    if (readReceiptsKind.value === "channel") {
      const jid = readReceiptsActiveRoomJid.value;
      if (!jid) return 0;
      return options.channelUnread.unreadForRoomJid(jid);
    }
    return 0;
  });

  useChatReadReceipts({
    isWindowFocused,
    isPinnedAtEdge: readReceiptsIsPinnedAtEdge,
    activeKind: readReceiptsKind,
    activeRoomJid: readReceiptsActiveRoomJid,
    activePeerJid: readReceiptsActivePeerJid,
    latestRemoteMessageId: readReceiptsLatestRemoteId,
    unreadCountForActive: readReceiptsUnreadCount,
    markChannelRead: (jid) => options.channelUnread.markRead(jid),
    markDmRead: (peer) => options.dmConversations.markRead(peer),
    markDisplayed: (id) => options.activeTarget.value.markDisplayed(id),
  });

  return {
    computedChannelUnreadMap,
    totalTabUnreadCount,
  };
}
