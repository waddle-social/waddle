import { computed, type ComputedRef, nextTick, type Ref, ref, watch } from "vue";
import type { useChannelMessages } from "@/channels/messages";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { useChatReadActivity } from "@/shell/read-activity";
import type { ChatShellState } from "@/shell/state";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid } from "@/lib/xmpp-client";
import { jidLocalpart } from "@/lib/xmpp/jid";
import {
  isTrustedManagedRoomJid,
  knownChannelIdForRoomJid,
  roomJidForChannelId as resolveRoomJidForChannelId,
} from "@/lib/channel-room";
import { navigate } from "@/router";
import type { ExtensionRouteKey } from "@/shell/controllers/use-extension-routes";
import type { ChannelLoadIntent } from "@/channels/room-access";

interface RoomSyncDeps {
  ui: ChatShellState;
  xmppClient: ComputedRef<BrowserXmppClient | null>;
  session: ComputedRef<WaddleSession | null>;
  waddles: ReturnType<typeof useWaddleDirectory>;
  messaging: ReturnType<typeof useChannelMessages>;
  dmConversations: ReturnType<typeof useDirectMessageConversations>;
  computedChannelUnreadMap: ReturnType<typeof useChatReadActivity>["computedChannelUnreadMap"];
  managedMucDomain: ComputedRef<string>;
  memberJidByNick: Ref<Record<string, string>>;
  activeExtensionRouteKey: Ref<ExtensionRouteKey | null>;
  cancelPendingRoute: () => void;
  updateUrl: () => void;
}

/**
 * Channel (MUC room) selection and the channel-id <-> room-JID
 * bookkeeping around it: the explicit per-selection room-JID map, the
 * pending selection for rooms whose channel hasn't been discovered yet,
 * and the watches that resolve either as structure data arrives.
 */
export function useRoomSync(deps: RoomSyncDeps) {
  const {
    ui,
    xmppClient,
    session,
    waddles,
    messaging,
    dmConversations,
    computedChannelUnreadMap,
    managedMucDomain,
    memberJidByNick,
    activeExtensionRouteKey,
    cancelPendingRoute,
    updateUrl,
  } = deps;

  const selectedChannelRoomJids = ref<Record<string, string>>({});
  const pendingChannelRoomJidSelection = ref<string | null>(null);
  function clearPendingChannelRoomJidSelection() {
    pendingChannelRoomJidSelection.value = null;
  }
  watch(xmppClient, (client, previous, onCleanup) => {
    if (previous && previous !== client) {
      selectedChannelRoomJids.value = {};
      clearPendingChannelRoomJidSelection();
    }
    if (!client) return;
    const unsubscribe = client.onRoomIdAmbiguous?.((id) => {
      const { [id]: _discard, ...remaining } = selectedChannelRoomJids.value;
      selectedChannelRoomJids.value = remaining;
      const pendingRoomJid = pendingChannelRoomJidSelection.value;
      if (pendingRoomJid && jidLocalpart(pendingRoomJid) === id) {
        clearPendingChannelRoomJidSelection();
      }
      if (waddles.activeChannelId.value === id) clearQuarantinedSelection();
    });
    if (unsubscribe) onCleanup(unsubscribe);
  }, { immediate: true, flush: "sync" });

  function clearQuarantinedSelection() {
    cancelPendingRoute();
    activeExtensionRouteKey.value = null;
    messaging.clearMessages();
    memberJidByNick.value = {};
  }
  // The directory's ambiguity subscriber clears selection before ours runs.
  // Observe that exact transition; an unrelated initial route also starts with
  // no active channel and must retain its request while discovery finishes.
  watch(waddles.activeChannelId, (id, previous) => {
    if (!id && previous && xmppClient.value?.isRoomIdAmbiguous?.(previous)) {
      clearQuarantinedSelection();
    }
  }, { flush: "sync" });

  const activeChannelRoomJid = computed(() => {
    const channelId = waddles.activeChannelId.value;
    const channel = waddles.currentChannel.value;
    if (!channelId || !session.value) return null;
    if (channel?.jid) return barePeerJid(channel.jid);
    const selectedRoomJid = selectedChannelRoomJids.value[channelId];
    if (selectedRoomJid) return selectedRoomJid;
    return resolveRoomJidForChannelId(session.value, waddles.channels.value, channelId);
  });
  watch(
    [activeChannelRoomJid, () => waddles.channels.value],
    ([roomJid]) => {
      if (ui.sidebarMode.value !== "channels" || !roomJid) return;
      const normalizedRoomJid = barePeerJid(roomJid);
      const channel = waddles.channels.value.find((candidate) =>
        candidate.jid ? barePeerJid(candidate.jid) === normalizedRoomJid : false
      );
      if (!channel || channel.id === waddles.activeChannelId.value) return;
      void selectChannel(channel.id, {
        roomJid: normalizedRoomJid,
        intent: "automatic",
      });
    },
  );
  watch(
    [() => waddles.channels.value, managedMucDomain],
    () => {
      const roomJid = pendingChannelRoomJidSelection.value;
      if (!roomJid) return;
      const channelId = knownChannelIdForRoomJid(
        roomJid,
        waddles.channels.value,
        managedMucDomain.value,
      );
      if (!channelId) return;
      pendingChannelRoomJidSelection.value = null;
      void selectChannel(channelId, { roomJid });
    },
  );

  function roomJidForChannelId(channelId: string): string | null {
    const sess = session.value;
    if (!sess || xmppClient.value?.isRoomIdAmbiguous?.(channelId)) return null;
    return resolveRoomJidForChannelId(sess, waddles.channels.value, channelId);
  }

  async function selectChannel(
    channelId: string,
    options: {
      roomJid?: string;
      surface?: "channels" | "dms";
      intent?: ChannelLoadIntent;
      /** The route already owns navigation, including explicit history retries. */
      fromRoute?: boolean;
    } = {},
  ) {
    xmppClient.value?.assertRoomIdAvailable?.(channelId);
    // An explicit JID is an observation, not permission to overwrite an ID.
    if (options.roomJid) xmppClient.value?.rememberRoomJidForChannel(channelId, barePeerJid(options.roomJid));
    if (options.intent !== "automatic" && !options.fromRoute) cancelPendingRoute();
    clearPendingChannelRoomJidSelection();
    ui.activePage.value = "chat";
    ui.activeCommunitySurface.value = null;
    ui.sidebarMode.value = options.surface ?? "channels";
    activeExtensionRouteKey.value = null;
    if (ui.sidebarMode.value !== "dms") dmConversations.closeDm();
    memberJidByNick.value = {};
    const selectedRoomJid = options.roomJid ? barePeerJid(options.roomJid) : null;
    if (selectedRoomJid) {
      selectedChannelRoomJids.value = {
        ...selectedChannelRoomJids.value,
        [channelId]: selectedRoomJid,
      };
      messaging.rememberChannelRoomJid(channelId, selectedRoomJid);
    }
    waddles.activeChannelId.value = channelId;
    // The panel watcher must clear the previous room's thread before URL sync.
    if (options.intent !== "automatic" && !options.fromRoute) void nextTick(updateUrl);
    void waddles.reloadChannelMembers(channelId);
    messaging.clearMessages();
    // XEP-0502: Clear activity indicator for this channel
    const roomJid = selectedRoomJid ?? roomJidForChannelId(channelId);
    if (roomJid) {
      messaging.clearChannelActivity(roomJid);
    }
    const unreadAtLoad = computedChannelUnreadMap.value[channelId]?.unread ?? 0;
    await messaging.loadMessages(
      waddles.currentChannel.value?.spaceId ?? "",
      channelId,
      unreadAtLoad,
      [],
      { intent: options.intent ?? "explicit-navigation" },
    );
    ui.showMobileNav.value = false;
  }

  async function selectChannelByRoomJid(roomJid: string) {
    const normalizedRoomJid = barePeerJid(roomJid);
    if (!normalizedRoomJid) return;
    const channelId = knownChannelIdForRoomJid(
      normalizedRoomJid,
      waddles.channels.value,
      managedMucDomain.value,
    );
    if (!channelId) {
      if (isTrustedManagedRoomJid(normalizedRoomJid, managedMucDomain.value)) {
        cancelPendingRoute();
        pendingChannelRoomJidSelection.value = normalizedRoomJid;
        ui.activePage.value = "chat";
        ui.sidebarMode.value = "channels";
        ui.activeCommunitySurface.value = null;
        activeExtensionRouteKey.value = null;
        dmConversations.closeDm();
        memberJidByNick.value = {};
        ui.showMobileNav.value = false;
      }
      return;
    }
    pendingChannelRoomJidSelection.value = null;
    await selectChannel(channelId, { roomJid: normalizedRoomJid });
  }

  async function selectGroupDm(
    roomJid: string,
    options: { updateUrl?: boolean; intent?: ChannelLoadIntent; fromRoute?: boolean } = {},
  ) {
    const normalizedRoomJid = barePeerJid(roomJid);
    const group = waddles.groupDms.value.find((candidate) => barePeerJid(candidate.roomJid) === normalizedRoomJid);
    const channelId = group?.id ?? knownChannelIdForRoomJid(
      normalizedRoomJid,
      waddles.groupDms.value.map((groupDm) => ({
        id: groupDm.id,
        name: groupDm.name,
        jid: groupDm.roomJid,
        isGroupDm: true,
      })),
      managedMucDomain.value,
    );
    if (!channelId) {
      if (options.intent !== "automatic" && !options.fromRoute) cancelPendingRoute();
      clearPendingChannelRoomJidSelection();
      ui.activePage.value = "chat";
      ui.activeCommunitySurface.value = null;
      ui.sidebarMode.value = "dms";
      ui.showPinnedPanel.value = false;
      activeExtensionRouteKey.value = null;
      dmConversations.closeDm();
      waddles.activeChannelId.value = null;
      ui.actionError.value = "Group message is not available yet.";
      navigate({ id: "dmList" }, { replace: true });
      return false;
    }
    dmConversations.closeDm();
    await selectChannel(channelId, {
      roomJid: normalizedRoomJid,
      surface: "dms",
      intent: options.intent ?? "explicit-navigation",
      fromRoute: options.fromRoute,
    });
    if (options.updateUrl !== false) updateUrl();
    return true;
  }

  return {
    selectedChannelRoomJids,
    pendingChannelRoomJidSelection,
    clearPendingChannelRoomJidSelection,
    activeChannelRoomJid,
    roomJidForChannelId,
    selectChannel,
    selectChannelByRoomJid,
    selectGroupDm,
  };
}
