import { type ComputedRef, type Ref, watch } from "vue";
import type { usePushNotifications } from "@/shell/notifications";
import type { createBrowserMessageTonePlayer } from "@/shell/audio-alerts";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { ChatShellState } from "@/shell/state";
import type { NotifySettingsStore } from "@/lib/notify-settings";
import type { BrowserXmppClient, LiveDmMessage, RoomActivityEvent } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import { parseManagedRoomBareJid } from "@/lib/xmpp-client";
import {
  showForegroundNotificationForDmActivity,
  showForegroundNotificationsForChannelActivities,
} from "@/shell/controllers/foreground-notifications";

interface NotificationOrchestrationDeps {
  ui: ChatShellState;
  xmppClient: ComputedRef<BrowserXmppClient | null>;
  session: ComputedRef<WaddleSession | null>;
  waddles: ReturnType<typeof useWaddleDirectory>;
  dmConversations: ReturnType<typeof useDirectMessageConversations>;
  notifications: ReturnType<typeof usePushNotifications>;
  notifySettings: NotifySettingsStore;
  messageSound: ReturnType<typeof createBrowserMessageTonePlayer>;
  isWindowFocused: Readonly<Ref<boolean>>;
  isSelfDoNotDisturb: () => boolean;
  pendingNotificationActivities: Ref<RoomActivityEvent[]>;
  selectChannel: (channelId: string) => Promise<void>;
  openDm: (peerJid: string) => Promise<void>;
}

/**
 * Foreground notification + push-subscription orchestration: drains the
 * channel-activity queue into banners/sounds, surfaces inbound DM
 * activity, and owns the push permission / enablement toggles.
 */
export function useNotificationOrchestration(deps: NotificationOrchestrationDeps) {
  const {
    ui,
    xmppClient,
    session,
    waddles,
    dmConversations,
    notifications,
    notifySettings,
    messageSound,
    isWindowFocused,
    isSelfDoNotDisturb,
    pendingNotificationActivities,
    selectChannel,
    openDm,
  } = deps;

  function resolveChannelNameFromJid(roomJid: string): string | null {
    const managedRoom = parseManagedRoomBareJid(roomJid);
    if (!managedRoom) return null;
    return waddles.channels.value.find((c) => c.id === managedRoom.channelId)?.name ?? null;
  }

  watch(() => pendingNotificationActivities.value, (events) => {
    if (events.length === 0) return;

    showForegroundNotificationsForChannelActivities(events, {
      notifySettings,
      notifications,
      messageSound,
      messageSoundsEnabled: () => notifications.messageSoundsEnabled.value,
      canShowForegroundNotification: () => notifications.canShowForegroundNotifications.value,
      isDoNotDisturb: isSelfDoNotDisturb,
      isTabFocused: () => isWindowFocused.value,
      sessionJid: session.value?.jid,
      resolveChannelNameFromJid,
      onNavigate: (roomJid) => {
        const managedRoom = parseManagedRoomBareJid(roomJid);
        if (!managedRoom) return;
        void selectChannel(managedRoom.channelId);
      },
    });

    pendingNotificationActivities.value = [];
  });

  /** Inbound DM surfaced by the connection layer — show a banner/sound
   * unless the user is looking at that conversation right now. */
  function notifyDmActivity(msg: LiveDmMessage): void {
    // MAM catch-up re-emissions (archive decodes) never notify: the
    // message may already have been shown live before a reconnect, and
    // replaying a banner/sound per re-fetched row spams the user.
    if (msg.createdAtSource === "archive") return;
    showForegroundNotificationForDmActivity(msg, {
      notifySettings,
      notifications,
      messageSound,
      messageSoundsEnabled: () => notifications.messageSoundsEnabled.value,
      canShowForegroundNotification: () => notifications.canShowForegroundNotifications.value,
      isDoNotDisturb: isSelfDoNotDisturb,
      isTabFocused: () => isWindowFocused.value,
      sessionJid: session.value?.jid,
      activePeerJid: ui.sidebarMode.value === "dms" ? dmConversations.activePeerJid.value : null,
      onNavigate: (peerJid) => {
        void openDm(peerJid);
      },
    });
  }

  async function setupPushSubscription() {
    if (!xmppClient.value || !session.value) return;
    await notifications.syncPushSubscription(xmppClient.value, session.value.jid);
  }

  async function handleRequestNotifications() {
    const state = await notifications.requestPermission();
    if (state === "granted") {
      await setupPushSubscription();
    }
  }

  async function handleToggleNotifications() {
    notifications.notificationsEnabled.value = !notifications.notificationsEnabled.value;
    if (notifications.notificationsEnabled.value) {
      await setupPushSubscription();
    } else if (xmppClient.value && session.value) {
      await notifications.disablePushSubscription(xmppClient.value, session.value.jid);
    }
  }

  function handleToggleMessageSounds() {
    notifications.messageSoundsEnabled.value = !notifications.messageSoundsEnabled.value;
  }

  return {
    notifyDmActivity,
    setupPushSubscription,
    handleRequestNotifications,
    handleToggleNotifications,
    handleToggleMessageSounds,
  };
}
