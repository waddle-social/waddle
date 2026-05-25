import { ref, watch } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import { getOrRegisterServiceWorker, registerServiceWorker as registerChatServiceWorker } from "@/lib/service-worker-registration";
import { createPushFlowLock } from "./push-flow-lock";
import { clearCachedVapidKey, loadVapidPublicKey } from "./vapid-cache";
import { withVapidRotationLock } from "./vapid-rotation-lock";

const STORAGE_KEY = "waddle.chat.notifications-enabled";
const DEVICE_ID_STORAGE_KEY = "waddle.chat.push-device-id";
const PUSH_NODE_STORAGE_KEY = "waddle.chat.push-node-id";
const PUSH_KID_STORAGE_KEY = "waddle.chat.push-kid";
const PUSH_SERVICE_JID = (import.meta.env.PUBLIC_WADDLE_XMPP_PUSH_SERVICE_JID ?? "").trim();
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
  /// Surfaced to the UI as a non-intrusive banner the first time a
  /// silent kid rotation re-binds the browser PushSubscription to a new
  /// VAPID key. The UI clears the flag once the banner is dismissed; we
  /// don't persist this across reloads because the rotation has already
  /// happened by then and re-showing would be noise.
  const rotationBannerVisible = ref(false);

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
    let keyBytes: Uint8Array;
    try {
      keyBytes = urlBase64ToUint8Array(vapidPublicKey);
    } catch (error) {
      console.warn("[notifications] urlBase64ToUint8Array failed on VAPID key:", error);
      return null;
    }
    // PushManager rejects `applicationServerKey` if the underlying
    // backing buffer has a non-zero `byteOffset` or extends beyond the
    // 65-byte uncompressed P-256 point — Firefox and Safari both
    // surface that as InvalidAccessError. Pass the Uint8Array directly
    // (PushManager accepts any BufferSource per spec) so the view's
    // length + offset stay authoritative, instead of leaking the raw
    // ArrayBuffer that may be larger.
    if (keyBytes.length !== 65 || keyBytes[0] !== 0x04) {
      // Belt-and-braces validation: the wasm parser already rejects
      // malformed keys, but a cache-hit path that bypassed the parser
      // (older code, race) could still reach this point. Refuse to
      // subscribe rather than hand the browser garbage.
      console.warn(
        "[notifications] VAPID public key is not a 65-byte 0x04-prefixed SEC1 point; refusing to subscribe",
      );
      return null;
    }
    try {
      return await reg.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: keyBytes,
      });
    } catch (error) {
      console.warn("[notifications] PushManager.subscribe() failed:", error);
      return null;
    }
  }

  /// Ensure the browser's PushSubscription is bound to the current
  /// server-advertised VAPID public key. Detects rotation (kid change),
  /// unsubscribes the stale subscription, and re-subscribes under the
  /// new key. Cross-tab serialized via the rotation lock so a single tab
  /// drives the re-subscribe sequence per rotation event.
  ///
  /// Returns the active subscription (post-rotation if any), or `null`
  /// when the server doesn't advertise a VAPID form (Web Push not
  /// configured on this deployment).
  async function ensureBrowserSubscriptionWithCurrentKey(
    xmppClient: BrowserXmppClient,
    userJid: string,
    serviceJid: string,
    reg: ServiceWorkerRegistration,
  ): Promise<{ subscription: PushSubscription; rotated: boolean } | null> {
    const accountJid = barePart(userJid);
    return withVapidRotationLock(accountJid, async () => {
      const advertisement = await loadVapidPublicKey({
        client: xmppClient,
        accountJid,
        serverJid: serviceJid,
      });
      if (!advertisement) {
        console.warn(
          "[notifications] Push Service does not advertise a VAPID public key; " +
          "Web Push subscription skipped. Foreground Notification API still works.",
        );
        return null;
      }
      const existing = await reg.pushManager.getSubscription();
      const persistedKid = loadPersistedKid();
      // Three rotation triggers:
      //   1. persistedKid present and differs from the advertised kid
      //      — server rotated under us.
      //   2. persistedKid missing while `existing` survives — localStorage
      //      was cleared (private mode, "Clear site data", new browser
      //      profile that inherited the SW registration). We can't trust
      //      the surviving subscription is bound to the current key, so
      //      verify via `existing.options.applicationServerKey` directly.
      //   3. The existing subscription's applicationServerKey doesn't
      //      match the advertised bytes — server-side rotation we missed.
      const existingKeyMatches =
        existing === null || subscriptionApplicationKeyMatches(existing, advertisement.publicKey);
      const kidChanged = persistedKid !== null && persistedKid !== advertisement.kid;
      if (existing && !kidChanged && existingKeyMatches) {
        persistKid(advertisement.kid);
        return { subscription: existing, rotated: false };
      }
      // Either no subscription yet, server kid changed under us, or the
      // surviving subscription is bound to a stale key. Unsubscribe (if
      // any) and re-subscribe under the new key.
      if (existing) {
        try {
          await existing.unsubscribe();
        } catch (error) {
          console.warn(
            "[notifications] PushManager.unsubscribe() failed during VAPID rotation; " +
            "continuing to re-subscribe under the new key:",
            error,
          );
        }
      }
      const fresh = await subscribeToPush(advertisement.publicKey);
      if (!fresh) {
        // Subscribe failed — drop the persisted kid + cache entry so the
        // next attempt re-fetches from the server. The chat can also fall
        // back to the foreground Notification API in this state.
        clearPersistedKid();
        clearCachedVapidKey(barePart(userJid), serviceJid);
        return null;
      }
      persistKid(advertisement.kid);
      return { subscription: fresh, rotated: kidChanged };
    });
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

    // Resolve the browser-side PushSubscription against the server's
    // currently-advertised VAPID public key. PR-D3 swapped the build-
    // time `PUBLIC_WADDLE_VAPID_PUBLIC_KEY` env for a runtime fetch via
    // the Push Service's XEP-0128 disco extension form so the chat can
    // ship without provider-specific build secrets and so server-side
    // VAPID rotations are picked up automatically.
    const reg = await getOrRegisterServiceWorker();
    if (!reg) {
      console.warn("[notifications] Service worker registration failed; push disabled");
      return false;
    }
    const subscribeResult = await ensureBrowserSubscriptionWithCurrentKey(
      xmppClient,
      userJid,
      serviceJid,
      reg,
    );
    if (!subscribeResult) {
      // Either the server doesn't advertise a VAPID form, or
      // pushManager.subscribe() rejected the key. The helper has
      // already emitted a precise warning; the chat falls back to the
      // foreground Notification API path.
      return false;
    }
    if (subscribeResult.rotated) {
      rotationBannerVisible.value = true;
    }
    const subscription = subscribeResult.subscription;

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
    // Browser unsubscribe is best-effort. If `pushManager.unsubscribe`
    // (or `getSubscription`) rejects — quota errors, transient
    // permissions glitch, browser bug — we MUST still continue to
    // the server-side `<disable-device>` IQ. Otherwise the user
    // leaves a stale device row on the Push Service that keeps
    // receiving fan-out from the (still-enabled) user-server.
    try {
      const reg = await getOrRegisterServiceWorker();
      if (reg) {
        const existing = await reg.pushManager.getSubscription();
        if (existing) {
          await existing.unsubscribe();
        }
      }
    } catch (error) {
      console.warn(
        "[notifications] Browser pushManager unsubscribe failed; " +
        "continuing to server-side disable-device:",
        error,
      );
    }

    const serviceJid = resolvePushServiceJid(userJid);
    if (!serviceJid) {
      // No Push Service JID means there was never an XMPP-side
      // registration to undo (`syncPushSubscriptionImpl` also bails
      // on `!serviceJid`). The browser subscription is already
      // gone above. Signal success so the caller updates the UI
      // correctly — consistent with the `!node || !deviceId` path
      // below. Round-7 Greptile P1 finding.
      console.warn(
        "[notifications] No XMPP Push Service JID resolved; " +
        "browser unsubscribed, no server-side state to remove",
      );
      return true;
    }

    const node = loadPushNodeId();
    const deviceId = loadDeviceId();

    // **Per-device opt-out only.** The user clicked "Disable
    // notifications" in THIS browser; this MUST NOT take down push
    // for other devices registered under the same per-(user, app-id)
    // node (e.g. a future iOS or Android install that #529/#530 will
    // wire in). XEP-0357's `<disable jid='push.<domain>' node='…'/>`
    // is node-level, not device-level — calling it here would
    // unsubscribe every device on the node. The Push Service's
    // `<disable-device device-id='…'/>` is the correct surface for
    // a single-device opt-out: it removes only this device's row,
    // and the user-server's `(jid, node)` registration stays alive
    // so other devices keep receiving fan-out.
    //
    // A "disable push everywhere" UI affordance (e.g. an account-
    // settings toggle that nukes the whole node + every device row)
    // is a separate flow; it should call `disablePushNotifications`
    // explicitly after the user has been told all their other
    // devices will stop receiving pushes too.
    if (!node || !deviceId) {
      console.warn(
        "[notifications] No locally-persisted node/deviceId; skipping disable-device. " +
        "The browser is unsubscribed; the Push Service row (if any) will be rotated " +
        "on the next enable flow.",
      );
      return true;
    }

    const disabled = await xmppClient.disablePushDevice({ serviceJid, node, deviceId });
    if (!disabled) {
      console.warn(
        "[notifications] disable-device IQ failed; this device may still be " +
        "registered with the Push Service",
      );
      return false;
    }
    // Clean up rotation state: drop the persisted kid + cached
    // advertisement so a re-enable starts from a fresh fetch. The
    // cache is keyed per `(account, server)` so this only invalidates
    // the entry for the disabling account, not any other tenant on the
    // same device.
    clearPersistedKid();
    clearCachedVapidKey(barePart(userJid), serviceJid);
    return true;
  }

  function dismissRotationBanner() {
    rotationBannerVisible.value = false;
  }

  return {
    permissionState,
    notificationsEnabled,
    rotationBannerVisible,
    dismissRotationBanner,
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

function persistKid(kid: string): void {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(PUSH_KID_STORAGE_KEY, kid);
}

function loadPersistedKid(): string | null {
  if (typeof window === "undefined") return null;
  return window.localStorage.getItem(PUSH_KID_STORAGE_KEY);
}

function clearPersistedKid(): void {
  if (typeof window === "undefined") return;
  window.localStorage.removeItem(PUSH_KID_STORAGE_KEY);
}

/// Strip the optional `/resource` from a full JID so the cache key is
/// stable across reconnect cycles (XMPP resources rotate every
/// connection). The bare-JID extraction mirrors `resolvePushServiceJid`'s
/// behavior — both must treat `alice@example.com/abc` and
/// `alice@example.com` as the same logical account.
function barePart(userJid: string): string {
  return userJid.split("/")[0] ?? userJid;
}

/// Compare an existing PushSubscription's applicationServerKey (the
/// 65-byte uncompressed P-256 point the browser stored when the
/// subscription was created) against the server's currently-advertised
/// public key (in base64url-no-pad form). Used to detect a stale
/// subscription whose kid we no longer have a record of — e.g. when
/// localStorage was wiped but the SW registration survived.
///
/// Returns `true` when the subscription has no applicationServerKey
/// (very old browsers / non-VAPID origin server endpoints) so callers
/// don't churn the subscription on an irrelevant comparison.
function subscriptionApplicationKeyMatches(
  subscription: PushSubscription,
  advertisedPublicKeyBase64Url: string,
): boolean {
  const existingKey = subscription.options.applicationServerKey;
  if (!existingKey) return true;
  let advertised: Uint8Array;
  try {
    advertised = urlBase64ToUint8Array(advertisedPublicKeyBase64Url);
  } catch {
    return false;
  }
  const existing =
    existingKey instanceof ArrayBuffer ? new Uint8Array(existingKey) : new Uint8Array(existingKey);
  if (existing.length !== advertised.length) return false;
  for (let i = 0; i < existing.length; i++) {
    if (existing[i] !== advertised[i]) return false;
  }
  return true;
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
