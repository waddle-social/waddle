import { ref, watch } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import { getOrRegisterServiceWorker, registerServiceWorker as registerChatServiceWorker } from "@/lib/service-worker-registration";
import { createPushFlowLock } from "./push-flow-lock";

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

  // Serialize `syncPushSubscription` and `disablePushSubscription`
  // against each other. Without this, a rapid Enable→Disable toggle
  // can interleave the multi-step flows: disable would skip
  // `pushManager.unsubscribe()` (no subscription yet), then enable
  // commits a `register-device` row AFTER disable already ran the
  // `disable-device` IQ, leaving a live device the user can't see
  // in the UI (`notificationsEnabled === false` but server-side
  // device row is registered). Round-4 hostile-client adversarial
  // review on PR #760.
  //
  // The lock helper is extracted into `push-flow-lock.ts` so the
  // mutation-resistance test suite can pin serialization without
  // having to stub the entire WASM + service-worker + Notification
  // API surface that `usePushNotifications` would otherwise need.
  const pushFlowLock = createPushFlowLock();

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
  function syncPushSubscription(
    xmppClient: BrowserXmppClient,
    userJid: string,
  ): Promise<boolean> {
    return pushFlowLock.run(() => syncPushSubscriptionImpl(xmppClient, userJid));
  }

  async function syncPushSubscriptionImpl(
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
    // Defense in depth: the WASM bridge hard-fails on empty IDs
    // (round-5 `require_non_empty_attr`), so an empty here would be
    // a regression — but a stale wasm bundle (built before that
    // fix) would have returned `{ id: "", … }`. Refuse to persist
    // and propagate as caller failure.
    if (!ensured.id) {
      console.warn(
        "[notifications] ensure-node returned an empty node id; refusing to persist",
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

  function disablePushSubscription(xmppClient: BrowserXmppClient, userJid: string): Promise<boolean> {
    return pushFlowLock.run(() => disablePushSubscriptionImpl(xmppClient, userJid));
  }

  async function disablePushSubscriptionImpl(xmppClient: BrowserXmppClient, userJid: string): Promise<boolean> {
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

    let node = loadPushNodeId();
    const deviceId = loadDeviceId();

    // Recover the node id if localStorage was cleared / never written
    // (e.g. user disabled before completing an enable, or switched
    // devices). `ensurePushNode` is idempotent on `(owner, app-id)`
    // so this is safe to call from disable.
    if (!node) {
      const ensured = await xmppClient.ensurePushNode({ serviceJid, appId: APP_ID_WEB });
      if (ensured) {
        node = ensured.id;
        persistPushNodeId(node);
      }
    }
    if (!node) {
      console.warn(
        "[notifications] No Push Service node id available; cannot disable XEP-0357",
      );
      return false;
    }

    // Best-effort: run BOTH the Push Service disable-device AND the
    // user-server XEP-0357 disable, even if the first one errors.
    // Stopping early on partial failure leaks state: the user-server
    // keeps publishing to a node whose Push Service device row is
    // already gone, or vice versa.
    //
    // Initialize `pushServiceDisabled` to `null` (= not attempted) so
    // a missing deviceId can't masquerade as a successful disable in
    // the final return value — round-5 Greptile P1 finding.
    let pushServiceDisabled: boolean | null = null;
    if (deviceId) {
      const disabled = await xmppClient.disablePushDevice({ serviceJid, node, deviceId });
      pushServiceDisabled = disabled !== null;
      if (!pushServiceDisabled) {
        console.warn(
          "[notifications] disable-device IQ failed; user-server XEP-0357 disable will still run",
        );
      }
    } else {
      console.warn(
        "[notifications] No deviceId in localStorage; skipping disable-device. " +
        "The Push Service device row may persist until a new enable flow rotates it.",
      );
    }
    const userServerDisabled = await xmppClient.disablePushNotifications({
      serviceJid,
      node,
    });
    if (!userServerDisabled) {
      console.warn(
        "[notifications] XEP-0357 disable IQ failed; Push Service publish jobs may still queue",
      );
    }
    // Only claim "fully disabled" when BOTH the user-server disable
    // AND the Push Service disable (if we attempted it) succeeded.
    // `null` (= disable-device not attempted because deviceId was
    // missing) collapses to `false` so the caller can't be misled
    // into thinking the Push Service row was cleaned up.
    return pushServiceDisabled === true && userServerDisabled;
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
  // Strip any `/resource` BEFORE extracting the domain — callers
  // sometimes pass a full JID (e.g. `connectionStore.session.jid`
  // which can carry the resource). Without the strip, the domain
  // becomes `example.com/resource` and the push service JID
  // becomes `push.example.com/resource` — an invalid component
  // address that breaks ensure-node / register-device / enable.
  // Round-5 Copilot review on PR #760.
  if (!userJid.includes("@")) return "";
  const bare = userJid.split("/")[0] ?? "";
  const domain = bare.split("@")[1] ?? "";
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
