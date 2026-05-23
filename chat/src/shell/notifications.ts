import { ref, watch } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import { getOrRegisterServiceWorker, registerServiceWorker as registerChatServiceWorker } from "@/lib/service-worker-registration";

const STORAGE_KEY = "waddle.chat.notifications-enabled";
const DEVICE_ID_STORAGE_KEY = "waddle.chat.push-device-id";
const PUSH_NODE_STORAGE_KEY = "waddle.chat.push-node-id";
const PUSH_SERVICE_JID = (import.meta.env.PUBLIC_WADDLE_XMPP_PUSH_SERVICE_JID ?? "").trim();
const VAPID_PUBLIC_KEY = (import.meta.env.PUBLIC_WADDLE_VAPID_PUBLIC_KEY ?? "").trim();
const NOTIFICATION_ICON_URL = "/android-chrome-192x192.png";

/// Push Service app-id for the browser/PWA chat. APNs ("ios") and
/// FCM ("android") live behind the same `<register-device>` shape
/// with their own app-ids — issues #529 / #530.
const APP_ID_WEB = "web";

/// Provider environment label for the Web Push device row. There's
/// no APNs-style dev/prod split for Web Push; the constant is
/// pinned here so the server's environment-filter logic is exercised.
const PUSH_ENVIRONMENT = "prod";

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

  /**
   * Full enable flow: ensure-node → register-device → XEP-0357 enable.
   *
   * Both calls to the Push Service (`urn:waddle:push-service:0`)
   * carry the actual browser PushSubscription credentials; the
   * downstream XEP-0357 `<enable jid='push.<domain>' node='…'/>`
   * to the user-server carries ONLY the service JID + node — no
   * endpoint, no p256dh, no auth. The Push Service is the single
   * owner of provider creds.
   */
  async function syncPushSubscription(
    xmppClient: BrowserXmppClient,
    userJid: string,
  ): Promise<boolean> {
    if (!notificationsEnabled.value || permissionState.value !== "granted") return false;

    const serviceJid = resolvePushServiceJid(userJid);
    if (!serviceJid) {
      console.warn("[notifications] No XMPP Push Service JID resolved; push disabled");
      return false;
    }

    // Get or create a browser-side PushSubscription. Without a VAPID
    // public key configured at build time the browser will refuse to
    // subscribe — log and bail so the caller can fall back to the
    // foreground Notification API.
    if (!VAPID_PUBLIC_KEY) {
      console.warn(
        "[notifications] PUBLIC_WADDLE_VAPID_PUBLIC_KEY is not set; Web Push subscription skipped. " +
        "Foreground Notification API still works while the chat tab is open.",
      );
      return false;
    }
    const reg = await getOrRegisterServiceWorker();
    if (!reg) {
      console.warn("[notifications] Service worker registration failed; push disabled");
      return false;
    }
    let subscription = await reg.pushManager.getSubscription();
    if (!subscription) {
      subscription = await subscribeToPush(VAPID_PUBLIC_KEY);
    }
    if (!subscription) {
      console.warn(
        "[notifications] PushManager.subscribe() failed (browser may have rejected VAPID key)",
      );
      return false;
    }

    const subJson = subscription.toJSON();
    const endpoint = subscription.endpoint;
    const auth = (subJson.keys?.auth ?? "").trim();
    const p256dh = (subJson.keys?.p256dh ?? "").trim();
    if (!endpoint || !auth || !p256dh) {
      console.warn(
        "[notifications] PushSubscription is missing endpoint/auth/p256dh; cannot register",
      );
      return false;
    }

    // Stable per-app PEP-style node id from the Push Service.
    const ensured = await xmppClient.ensurePushNode({ serviceJid, appId: APP_ID_WEB });
    if (!ensured) {
      console.warn(
        "[notifications] ensure-node IQ failed; XMPP Push Service may be unreachable",
      );
      return false;
    }
    persistPushNodeId(ensured.id);

    const deviceId = ensureDeviceId();
    const registered = await xmppClient.registerWebPushDevice({
      serviceJid,
      node: ensured.id,
      deviceId,
      environment: PUSH_ENVIRONMENT,
      providerEndpoint: endpoint,
      providerToken: auth,
      providerKeyMaterial: p256dh,
    });
    if (!registered) {
      console.warn(
        "[notifications] register-device IQ failed; push subscription will not be delivered",
      );
      return false;
    }

    const enabled = await xmppClient.enablePushNotifications({
      serviceJid,
      node: ensured.id,
    });
    if (!enabled) {
      console.warn(
        "[notifications] XEP-0357 enable IQ failed; device is registered but not advertised",
      );
    }
    return enabled;
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
    if (!serviceJid) {
      console.warn("[notifications] No XMPP Push Service JID resolved on disable");
      return false;
    }

    const node = loadPushNodeId();
    const deviceId = loadDeviceId();

    // Best-effort: run BOTH the Push Service disable-device AND the
    // user-server XEP-0357 disable, even if the first one errors.
    // Stopping early on partial failure leaks state: the user-server
    // keeps publishing to a node whose Push Service device row is
    // already gone, or vice versa.
    let pushServiceDisabled = true;
    if (node && deviceId) {
      pushServiceDisabled = await xmppClient.disablePushDevice({ serviceJid, node, deviceId });
      if (!pushServiceDisabled) {
        console.warn(
          "[notifications] disable-device IQ failed; user-server XEP-0357 disable will still run",
        );
      }
    }
    if (!node) return false;
    const userServerDisabled = await xmppClient.disablePushNotifications({
      serviceJid,
      node,
    });
    if (!userServerDisabled) {
      console.warn(
        "[notifications] XEP-0357 disable IQ failed; Push Service publish jobs may still queue",
      );
    }
    return pushServiceDisabled && userServerDisabled;
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

function ensureDeviceId(): string {
  if (typeof window === "undefined") {
    return `web-${Math.random().toString(36).slice(2)}`;
  }
  const existing = window.localStorage.getItem(DEVICE_ID_STORAGE_KEY);
  if (existing && existing.length > 0) return existing;
  const minted = `web-${crypto.randomUUID()}`;
  window.localStorage.setItem(DEVICE_ID_STORAGE_KEY, minted);
  return minted;
}

function loadDeviceId(): string | null {
  if (typeof window === "undefined") return null;
  return window.localStorage.getItem(DEVICE_ID_STORAGE_KEY);
}

function persistPushNodeId(node: string): void {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(PUSH_NODE_STORAGE_KEY, node);
}

function loadPushNodeId(): string | null {
  if (typeof window === "undefined") return null;
  return window.localStorage.getItem(PUSH_NODE_STORAGE_KEY);
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
