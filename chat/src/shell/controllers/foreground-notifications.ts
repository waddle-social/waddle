import { shouldShowChannelForegroundNotification } from "@/shell/notifications";
import { barePeerJid, type LiveDmMessage, type RoomActivityEvent } from "@/lib/xmpp-client";
import type { NotifySettingsStore } from "@/lib/notify-settings";
import { mentionMatchesBareJid } from "@/lib/mentions";

export interface ChannelActivityNotificationDeps {
  notifySettings: Pick<NotifySettingsStore, "getMode">;
  notifications: {
    showMentionNotification: (opts: {
      senderNick: string;
      channelName: string;
      body: string;
      roomJid: string;
      isBroadcast: boolean;
      stanzaId?: string;
      onNavigate?: (roomJid: string) => void;
    }) => void;
    showChannelMessageNotification: (opts: {
      senderNick: string;
      channelName: string;
      body: string;
      roomJid: string;
      stanzaId?: string;
      onNavigate?: (roomJid: string) => void;
    }) => void;
  };
  messageSound?: {
    play: (key: string) => void | Promise<void>;
  };
  messageSoundsEnabled?: () => boolean;
  canShowForegroundNotification?: () => boolean;
  isDoNotDisturb?: () => boolean;
  isTabFocused?: () => boolean;
  sessionJid: string | null | undefined;
  resolveChannelNameFromJid: (roomJid: string) => string | null;
  onNavigate: (roomJid: string) => void;
}

export interface DmActivityNotificationDeps {
  notifySettings: Pick<NotifySettingsStore, "getMode">;
  notifications: {
    showDmNotification: (opts: {
      senderUsername: string;
      peerJid: string;
      body: string;
      stanzaId?: string;
      onNavigate?: (peerJid: string) => void;
    }) => void;
  };
  messageSound?: {
    play: (key: string) => void | Promise<void>;
  };
  messageSoundsEnabled?: () => boolean;
  canShowForegroundNotification?: () => boolean;
  isDoNotDisturb?: () => boolean;
  isTabFocused?: () => boolean;
  sessionJid: string | null | undefined;
  activePeerJid: string | null | undefined;
  onNavigate: (peerJid: string) => void;
}

export function showForegroundNotificationForChannelActivity(
  event: RoomActivityEvent,
  deps: ChannelActivityNotificationDeps,
): void {
  const channelName = deps.resolveChannelNameFromJid(event.roomJid) ?? "unknown";
  const isBroadcast = !!event.broadcastMention;
  const isPersonalMention = event.mentions?.some((mention) =>
    mentionMatchesBareJid(mention, deps.sessionJid)
  ) ?? false;
  const isMention = isBroadcast || isPersonalMention;
  const mode = deps.notifySettings.getMode(event.roomJid, "private-group");

  if (!shouldShowChannelForegroundNotification({ mode, isMention })) return;
  if (deps.isDoNotDisturb?.() === true) return;
  if (deps.canShowForegroundNotification?.() === false) return;

  if (deps.isTabFocused?.() === false && deps.messageSoundsEnabled?.() !== false) {
    void deps.messageSound?.play(messageSoundKey(event.roomJid, event.stanzaId));
  }

  if (isMention) {
    deps.notifications.showMentionNotification({
      senderNick: event.nick,
      channelName,
      body: event.body,
      roomJid: event.roomJid,
      isBroadcast,
      stanzaId: event.stanzaId,
      onNavigate: deps.onNavigate,
    });
    return;
  }

  deps.notifications.showChannelMessageNotification({
    senderNick: event.nick,
    channelName,
    body: event.body,
    roomJid: event.roomJid,
    stanzaId: event.stanzaId,
    onNavigate: deps.onNavigate,
  });
}

export function showForegroundNotificationsForChannelActivities(
  events: RoomActivityEvent[],
  deps: ChannelActivityNotificationDeps,
): void {
  for (const event of events) {
    showForegroundNotificationForChannelActivity(event, deps);
  }
}

export function showForegroundNotificationForDmActivity(
  message: LiveDmMessage,
  deps: DmActivityNotificationDeps,
): void {
  const isSelf = barePeerJid(message.fromJid) === barePeerJid(deps.sessionJid ?? "");
  const isViewingThisDm = deps.activePeerJid === message.peerJid;
  if (isSelf || isViewingThisDm) return;

  const mode = deps.notifySettings.getMode(message.peerJid, "direct-chat");
  const isMention = message.mentions?.some((mention) =>
    mentionMatchesBareJid(mention, deps.sessionJid)
  ) ?? false;
  if (!shouldShowChannelForegroundNotification({ mode, isMention })) return;
  if (deps.isDoNotDisturb?.() === true) return;
  if (deps.canShowForegroundNotification?.() === false) return;

  if (deps.isTabFocused?.() === false && deps.messageSoundsEnabled?.() !== false) {
    void deps.messageSound?.play(messageSoundKey(message.peerJid, message.stanzaId ?? message.id));
  }

  deps.notifications.showDmNotification({
    senderUsername: message.nick,
    peerJid: message.peerJid,
    body: message.body,
    stanzaId: message.stanzaId,
    onNavigate: deps.onNavigate,
  });
}

function messageSoundKey(conversationJid: string, stanzaId: string | undefined): string {
  return `message:${conversationJid}:${stanzaId ?? createUnstampedMessageSoundId()}`;
}

function createUnstampedMessageSoundId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return `unstamped-${crypto.randomUUID()}`;
  }
  return `unstamped-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}
