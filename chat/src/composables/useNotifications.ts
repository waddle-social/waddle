import { ref, watch } from "vue";

const STORAGE_KEY = "waddle.chat.notifications-enabled";

const hasNotificationApi =
  typeof window !== "undefined" && "Notification" in window;

export interface MentionNotificationOptions {
  senderNick: string;
  channelName: string;
  body: string;
  roomJid: string;
  isBroadcast: boolean;
  onNavigate?: (roomJid: string) => void;
}

export function useNotifications() {
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
      icon: "/favicon.svg",
    });

    notification.onclick = () => {
      window.focus();
      opts.onNavigate?.(opts.roomJid);
      notification.close();
    };

    setTimeout(() => notification.close(), 5000);
  }

  // -- Service worker + Web Push --

  let swRegistration: ServiceWorkerRegistration | null = null;

  async function registerServiceWorker(): Promise<ServiceWorkerRegistration | null> {
    if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) return null;
    try {
      swRegistration = await navigator.serviceWorker.register("/sw.js");
      return swRegistration;
    } catch {
      return null;
    }
  }

  async function subscribeToPush(
    vapidPublicKey: string,
  ): Promise<PushSubscription | null> {
    const reg = swRegistration ?? (await registerServiceWorker());
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

  return {
    permissionState,
    notificationsEnabled,
    requestPermission,
    showMentionNotification,
    registerServiceWorker,
    subscribeToPush,
  };
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
