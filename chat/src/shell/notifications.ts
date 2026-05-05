import { ref, watch } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import { getOrRegisterServiceWorker, registerServiceWorker as registerChatServiceWorker } from "@/lib/service-worker-registration";

const STORAGE_KEY = "waddle.chat.notifications-enabled";
const VAPID_PUBLIC_KEY = (import.meta.env.PUBLIC_WADDLE_WEB_PUSH_VAPID_PUBLIC_KEY ?? "").trim();
const PUSH_SERVICE_JID = (import.meta.env.PUBLIC_WADDLE_XMPP_PUSH_SERVICE_JID ?? "").trim();
const NOTIFICATION_ICON_URL = "/android-chrome-192x192.png";

const hasNotificationApi =
  typeof window !== "undefined" && "Notification" in window;

interface MentionNotificationOptions {
  senderNick: string;
  channelName: string;
  body: string;
  roomJid: string;
  isBroadcast: boolean;
  onNavigate?: (roomJid: string) => void;
}

interface DmNotificationOptions {
  senderUsername: string;
  peerJid: string;
  body: string;
  onNavigate?: (peerJid: string) => void;
}

export function usePushNotifications() {
  const permissionState = ref<NotificationPermission>(
    hasNotificationApi ? Notification.permission : "denied",
  );
  const notificationsEnabled = ref(loadEnabled());

  watch(notificationsEnabled, (v) => {
    if (typeof window !== "undefined") {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(v));
    }
  });

  async function requestPermission(): Promise<NotificationPermission> {
    if (!hasNotificationApi) return "denied";
    const result = await Notification.requestPermission();
    permissionState.value = result;
    if (result === "granted") {
      notificationsEnabled.value = true;
    }
    return result;
  }

  function showMentionNotification(opts: MentionNotificationOptions) {
    if (!hasNotificationApi) return;
    if (permissionState.value !== "granted" || !notificationsEnabled.value) return;

    const title = opts.isBroadcast
      ? `@everyone in #${opts.channelName}`
      : `@${opts.senderNick} in #${opts.channelName}`;
    const body =
      opts.body.length > 100 ? `${opts.body.slice(0, 100)}…` : opts.body;

    const notification = new Notification(title, {
      body,
      tag: opts.roomJid,
      icon: NOTIFICATION_ICON_URL,
    });

    notification.onclick = () => {
      window.focus();
      opts.onNavigate?.(opts.roomJid);
      notification.close();
    };

    setTimeout(() => notification.close(), 5000);
  }

  function showDmNotification(opts: DmNotificationOptions) {
    if (!hasNotificationApi) return;
    if (permissionState.value !== "granted" || !notificationsEnabled.value) return;
    const body = opts.body.length > 100 ? `${opts.body.slice(0, 100)}…` : opts.body;
    const notification = new Notification(`Message from @${opts.senderUsername}`, {
      body,
      tag: opts.peerJid,
      icon: NOTIFICATION_ICON_URL,
    });
    notification.onclick = () => {
      window.focus();
      opts.onNavigate?.(opts.peerJid);
      notification.close();
    };
    setTimeout(() => notification.close(), 5000);
  }

  // -- Service worker + Web Push --

  async function registerServiceWorker(): Promise<ServiceWorkerRegistration | null> {
    return registerChatServiceWorker();
  }

  async function subscribeToPush(
    vapidPublicKey: string,
  ): Promise<PushSubscription | null> {
    const reg = await getOrRegisterServiceWorker();
    if (!reg) return null;
    try {
      const keyBytes = urlBase64ToUint8Array(vapidPublicKey);
      const sub = await reg.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: keyBytes.buffer as ArrayBuffer,
      });
      return sub;
    } catch {
      return null;
    }
  }

  async function syncPushSubscription(xmppClient: BrowserXmppClient, userJid: string): Promise<boolean> {
    if (!notificationsEnabled.value || permissionState.value !== "granted") return false;
    if (!VAPID_PUBLIC_KEY) return false;

    const sub = await subscribeToPush(VAPID_PUBLIC_KEY);
    if (!sub) return false;

    const subJson = sub.toJSON();
    const endpoint = subJson.endpoint ?? sub.endpoint;
    const p256dh = subJson.keys?.p256dh;
    const auth = subJson.keys?.auth;
    if (!endpoint || !p256dh || !auth) return false;

    const serviceJid = resolvePushServiceJid(userJid);
    if (!serviceJid) return false;

    return xmppClient.enablePushNotifications({
      serviceJid,
      node: "web-push",
      endpoint,
      p256dh,
      auth,
    });
  }

  async function disablePushSubscription(xmppClient: BrowserXmppClient, userJid: string): Promise<boolean> {
    const reg = await getOrRegisterServiceWorker();
    if (reg) {
      const existing = await reg.pushManager.getSubscription();
      if (existing) {
        await existing.unsubscribe();
      }
    }

    const serviceJid = resolvePushServiceJid(userJid);
    if (!serviceJid) return false;

    return xmppClient.disablePushNotifications({
      serviceJid,
      node: "web-push",
    });
  }

  return {
    permissionState,
    notificationsEnabled,
    requestPermission,
    showMentionNotification,
    showDmNotification,
    registerServiceWorker,
    subscribeToPush,
    syncPushSubscription,
    disablePushSubscription,
  };
}

function resolvePushServiceJid(userJid: string): string {
  const domain = userJid.includes("@") ? userJid.split("@")[1] ?? "" : "";
  return PUSH_SERVICE_JID || (domain ? `push.${domain}` : "");
}

function loadEnabled(): boolean {
  if (typeof window === "undefined") return false;
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    return raw ? JSON.parse(raw) === true : false;
  } catch {
    return false;
  }
}

function urlBase64ToUint8Array(base64String: string): Uint8Array {
  const padding = "=".repeat((4 - (base64String.length % 4)) % 4);
  const base64 = (base64String + padding).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(base64);
  const arr = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) {
    arr[i] = raw.charCodeAt(i);
  }
  return arr;
}
