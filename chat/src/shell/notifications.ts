import { computed, ref, watch } from "vue";
import type { BrowserXmppClient, NotifyMode } from "@/lib/xmpp-client";
import { getOrRegisterServiceWorker, registerServiceWorker as registerChatServiceWorker } from "@/lib/service-worker-registration";
import { createPushFlowLock } from "./push-flow-lock";
import {
  clearDeviceId,
  clearPushNodeId,
  loadDeviceId,
  loadPushNodeId,
  persistDeviceId,
  persistPushNodeId,
} from "./push-device-store";
import { clearCachedVapidKey, loadVapidPublicKey } from "./vapid-cache";
import { withVapidRotationLock } from "./vapid-rotation-lock";

const STORAGE_KEY = "waddle.chat.notifications-enabled";
const MESSAGE_SOUNDS_STORAGE_KEY = "waddle.chat.message-sounds-enabled";
/// Prefix for the per-(account, service) persisted VAPID kid. The
/// full key is built by `pushKidStorageKey(accountJid, serviceJid)`.
/// Multi-account sessions MUST NOT share one global kid — without
/// namespacing, account B's kid would overwrite account A's,
/// causing spurious rotation triggers when A reconnects and reads
/// B's kid back as "different from advertised".
const PUSH_KID_STORAGE_PREFIX = "waddle.chat.push-kid:";
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

function hasNotificationApi(): boolean {
  return typeof window !== "undefined" && "Notification" in window;
}

interface MentionNotificationOptions {
  senderNick: string;
  channelName: string;
  body: string;
  roomJid: string;
  isBroadcast: boolean;
  /** XEP-0359 stanza id of the originating message. When present, the
   * foreground tab posts it to the active service worker via
   * `waddle:item-shown` so a Web Push for the same id (arriving a few
   * tens of ms later) is suppressed instead of double-firing. */
  stanzaId?: string;
  onNavigate?: (roomJid: string) => void;
}

interface ChannelMessageNotificationOptions {
  senderNick: string;
  channelName: string;
  body: string;
  roomJid: string;
  stanzaId?: string;
  onNavigate?: (roomJid: string) => void;
}

interface DmNotificationOptions {
  senderUsername: string;
  peerJid: string;
  body: string;
  /** XEP-0359 stanza id; see `MentionNotificationOptions.stanzaId`. */
  stanzaId?: string;
  onNavigate?: (peerJid: string) => void;
}

export function shouldShowChannelForegroundNotification(opts: {
  mode: NotifyMode;
  isMention: boolean;
}): boolean {
  switch (opts.mode) {
    case "always":
      return true;
    case "on-mention":
      return opts.isMention;
    case "never":
      return false;
  }
}

/// Foreground→SW signal: tell the active service worker we just
/// rendered a notification for this stanza id so it can suppress the
/// matching Web Push when it lands. Best-effort: a missing controller
/// (SW not active yet, or never installed) is silently ignored —
/// without the SW running, there is no push handler to deliver a
/// duplicate notification either.
const SW_ITEM_SHOWN_MESSAGE_TYPE = "waddle:item-shown";
function postItemShownToServiceWorker(itemId: string | undefined): void {
  if (!itemId) return;
  if (typeof navigator === "undefined" || !navigator.serviceWorker) return;
  const controller = navigator.serviceWorker.controller;
  if (!controller) return;
  try {
    controller.postMessage({ type: SW_ITEM_SHOWN_MESSAGE_TYPE, itemId });
  } catch {
    // postMessage can throw if the controller transitioned to
    // redundant between the controller-check and the call.
  }
}

export function usePushNotifications() {
  const permissionState = ref<NotificationPermission>(
    hasNotificationApi() ? Notification.permission : "denied",
  );
  const notificationsEnabled = ref(loadEnabled());
  const messageSoundsEnabled = ref(loadMessageSoundsEnabled());
  const canShowForegroundNotifications = computed(() =>
    permissionState.value === "granted" && notificationsEnabled.value,
  );
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
      try {
        window.localStorage.setItem(STORAGE_KEY, JSON.stringify(v));
      } catch (error) {
        console.warn("Unable to persist notification preference", error);
      }
    }
  });

  watch(messageSoundsEnabled, (v) => {
    if (typeof window !== "undefined") {
      try {
        window.localStorage.setItem(MESSAGE_SOUNDS_STORAGE_KEY, JSON.stringify(v));
      } catch (error) {
        console.warn("Unable to persist message sound preference", error);
      }
    }
  });

  async function requestPermission(): Promise<NotificationPermission> {
    if (!hasNotificationApi()) return "denied";
    const result = await Notification.requestPermission();
    permissionState.value = result;
    if (result === "granted") {
      notificationsEnabled.value = true;
    }
    return result;
  }

  function showMentionNotification(opts: MentionNotificationOptions) {
    if (!hasNotificationApi()) return;
    if (permissionState.value !== "granted" || !notificationsEnabled.value) return;

    const title = opts.isBroadcast
      ? `@everyone in #${opts.channelName}`
      : `@${opts.senderNick} in #${opts.channelName}`;
    const body =
      opts.body.length > 100 ? `${opts.body.slice(0, 100)}…` : opts.body;

    const notification = new Notification(title, {
      body,
      // Per-room tag intentionally lets the browser replace an older
      // visible banner for the same channel while every stanza id still
      // posts to the service worker for Web Push dedup below.
      tag: opts.roomJid,
      icon: NOTIFICATION_ICON_URL,
    });

    notification.onclick = () => {
      window.focus();
      opts.onNavigate?.(opts.roomJid);
      notification.close();
    };

    // Foreground→SW dedup signal: post BEFORE the setTimeout-close so
    // the SW Map records the id even if the user dismisses the banner
    // before the SW push arrives.
    postItemShownToServiceWorker(opts.stanzaId);

    setTimeout(() => notification.close(), 5000);
  }

  function showChannelMessageNotification(opts: ChannelMessageNotificationOptions) {
    if (!hasNotificationApi()) return;
    if (permissionState.value !== "granted" || !notificationsEnabled.value) return;

    const body = opts.body.length > 100 ? `${opts.body.slice(0, 100)}…` : opts.body;
    const notification = new Notification(`@${opts.senderNick} in #${opts.channelName}`, {
      body,
      // Match mention notifications: one visible banner per room, with
      // per-message Web Push dedup still handled by stanza id below.
      tag: opts.roomJid,
      icon: NOTIFICATION_ICON_URL,
    });

    notification.onclick = () => {
      window.focus();
      opts.onNavigate?.(opts.roomJid);
      notification.close();
    };

    postItemShownToServiceWorker(opts.stanzaId);
    setTimeout(() => notification.close(), 5000);
  }

  function showDmNotification(opts: DmNotificationOptions) {
    if (!hasNotificationApi()) return;
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
    postItemShownToServiceWorker(opts.stanzaId);
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
    return subscribeOnRegistration(reg, vapidPublicKey);
  }

  /// Subscribe `reg.pushManager` to Web Push for the given VAPID
  /// public key. Split out from `subscribeToPush` so callers that
  /// already hold a `ServiceWorkerRegistration` (the rotation path)
  /// don't pay an extra `getOrRegisterServiceWorker()` round-trip —
  /// that round-trip can spuriously fail under an SW-update race
  /// while we hold the rotation lock.
  async function subscribeOnRegistration(
    reg: ServiceWorkerRegistration,
    vapidPublicKey: string,
  ): Promise<PushSubscription | null> {
    let keyBytes: Uint8Array;
    try {
      keyBytes = urlBase64ToUint8Array(vapidPublicKey);
    } catch (error) {
      console.warn("[notifications] urlBase64ToUint8Array failed on VAPID key:", error);
      return null;
    }
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
    // Re-pack into a fresh `Uint8Array<ArrayBuffer>` view (NOT
    // `Uint8Array<ArrayBufferLike>`, which TypeScript widens any
    // `Uint8Array` to and which `PushSubscriptionOptionsInit.applicationServerKey`
    // rejects because `SharedArrayBuffer` isn't a valid backing
    // store). The new ArrayBuffer is owned by this view exclusively —
    // no offset, exact 65-byte length — which is also the shape
    // Firefox and Safari validate against; passing a wider backing
    // buffer surfaces as InvalidAccessError in those engines.
    const exactKey = new Uint8Array(new ArrayBuffer(keyBytes.byteLength));
    exactKey.set(keyBytes);
    try {
      return await reg.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: exactKey,
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
      // Force-refresh while holding the rotation lock so a cache-hit
      // path doesn't mask a real server-side VAPID rotation that
      // landed inside the cache TTL window. The 1h cache TTL is a
      // backstop for stale tabs; rotation detection MUST run against
      // a fresh disco fetch every time the lock is taken (i.e. on
      // every enable / reconnect).
      const advertisement = await loadVapidPublicKey({
        client: xmppClient,
        accountJid,
        serverJid: serviceJid,
        forceRefresh: true,
      });
      if (!advertisement) {
        console.warn(
          "[notifications] Push Service does not advertise a VAPID public key; " +
          "Web Push subscription skipped. Foreground Notification API still works.",
        );
        return null;
      }
      const existing = await reg.pushManager.getSubscription();
      const persistedKid = loadPersistedKid(accountJid, serviceJid);
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
        persistKid(accountJid, serviceJid, advertisement.kid);
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
      // Re-use the registration the caller already holds instead of
      // re-fetching via `getOrRegisterServiceWorker()`. The double
      // lookup could turn a recoverable state (we have `reg` in hand)
      // into a `null` if the second async lookup raced an SW update.
      const fresh = await subscribeOnRegistration(reg, advertisement.publicKey);
      if (!fresh) {
        // Subscribe failed — drop the persisted kid + cache entry so the
        // next attempt re-fetches from the server. The chat can also fall
        // back to the foreground Notification API in this state.
        clearPersistedKid(accountJid, serviceJid);
        clearCachedVapidKey(accountJid, serviceJid);
        return null;
      }
      persistKid(accountJid, serviceJid, advertisement.kid);
      return { subscription: fresh, rotated: kidChanged };
    });
  }

  /**
   * Full enable flow: XEP-0050 `register-device` on the Push Service
   * (allocates + binds in one multi-step round trip) → XEP-0357
   * `<enable/>` on the user-server.
   *
   * `register-device` carries the actual browser PushSubscription
   * credentials; the downstream XEP-0357
   * `<enable jid='push.<domain>' node='…'/>` to the user-server
   * carries ONLY the service JID + node — no endpoint, no p256dh,
   * no auth. The Push Service is the single owner of provider creds.
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
    const accountJid = barePart(userJid);

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

    // XEP-0050 multi-step `register-device` allocates the per-(user,
    // app-id) push node, binds the browser's PushSubscription, and
    // returns BOTH the assigned XEP-0357 node id and the Push
    // Service-assigned device id in the stage-4 result form.
    const registered = await xmppClient.registerPushDevice({
      serviceJid,
      appId: APP_ID_WEB,
      environment: PUSH_ENVIRONMENT,
      endpoint,
      p256dh,
      auth,
    });
    if (!registered) {
      console.warn(
        "[notifications] XEP-0050 register-device failed; push subscription will not be delivered",
      );
      // Round-3 adversarial finding: when register-device fails AFTER
      // a previous successful enable persisted node + deviceId, the
      // stale ids would still drive a doomed `disable-device` round
      // against a node the server may have rotated. Clear them so
      // the next opt-out is a clean no-op rather than surfacing a
      // server-side `item-not-found` to the user.
      clearPushNodeId(accountJid, serviceJid);
      clearDeviceId(accountJid, serviceJid);
      return false;
    }
    persistPushNodeId(accountJid, serviceJid, registered.node);
    // Persist the server-assigned device id so the per-device
    // `disable-device` flow can later scope to this exact row.
    persistDeviceId(accountJid, serviceJid, registered.deviceId);

    const enabled = await xmppClient.enablePushNotifications({
      serviceJid,
      node: registered.node,
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

    const accountJid = barePart(userJid);
    const node = loadPushNodeId(accountJid, serviceJid);
    const deviceId = loadDeviceId(accountJid, serviceJid);

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
      // KEEP the persisted node/deviceId on failure. The browser is
      // already unsubscribed, but the server-side `disable-device` did
      // NOT land, so the device row is still active. Clearing the ids
      // here would strand that active row with no client-side handle to
      // ever retry the opt-out (the `!node || !deviceId` guard above
      // would short-circuit the next attempt as a no-op). Holding the
      // ids lets a retry — or the next disable click — re-issue the IQ
      // against the same row.
      console.warn(
        "[notifications] disable-device IQ failed; this device may still be " +
        "registered with the Push Service. Retaining node/deviceId so the " +
        "opt-out can be retried.",
      );
      return false;
    }
    // Success: the device row is now disabled, so the persisted ids no
    // longer describe a live registration. Drop them together with the
    // rotation kid + cached VAPID advertisement so a later re-enable
    // starts from a clean fetch. All keys are per-(account, service), so
    // this only invalidates the disabling pair, not any other tenant on
    // the same device. (Previously only the kid + cache were cleared
    // here, leaking a stale node/deviceId — adversarial finding.)
    clearPushNodeId(accountJid, serviceJid);
    clearDeviceId(accountJid, serviceJid);
    clearPersistedKid(accountJid, serviceJid);
    clearCachedVapidKey(accountJid, serviceJid);
    return true;
  }

  function dismissRotationBanner() {
    rotationBannerVisible.value = false;
  }

  return {
    permissionState,
    notificationsEnabled,
    messageSoundsEnabled,
    canShowForegroundNotifications,
    rotationBannerVisible,
    dismissRotationBanner,
    requestPermission,
    showMentionNotification,
    showChannelMessageNotification,
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
  // address that breaks both XEP-0050 push commands and the
  // user-server XEP-0357 enable. Round-5 Copilot review on PR #760.
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

function loadMessageSoundsEnabled(): boolean {
  if (typeof window === "undefined") return true;
  try {
    const raw = window.localStorage.getItem(MESSAGE_SOUNDS_STORAGE_KEY);
    return raw ? JSON.parse(raw) === true : true;
  } catch {
    return true;
  }
}

/// Build the per-(account, service) localStorage key for the
/// persisted VAPID kid. `JSON.stringify` on a two-element array gives
/// an unambiguous encoding regardless of the individual values'
/// content (JIDs legally contain `:` and most other plausible
/// separators), matching the namespacing rule used by `vapid-cache.ts`.
function pushKidStorageKey(accountJid: string, serviceJid: string): string {
  return `${PUSH_KID_STORAGE_PREFIX}${JSON.stringify([accountJid, serviceJid])}`;
}

function persistKid(accountJid: string, serviceJid: string, kid: string): void {
  if (typeof window === "undefined") return;
  // Safari Lockdown Mode and some Firefox/Brave privacy configurations
  // throw a SecurityError on `window.localStorage` access (not just on
  // get/set). Wrap so a hardened browser doesn't kill the entire
  // rotation flow on a single persistence call.
  try {
    window.localStorage.setItem(pushKidStorageKey(accountJid, serviceJid), kid);
  } catch (error) {
    console.warn("[notifications] localStorage unavailable; kid not persisted:", error);
  }
}

function loadPersistedKid(accountJid: string, serviceJid: string): string | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage.getItem(pushKidStorageKey(accountJid, serviceJid));
  } catch {
    return null;
  }
}

function clearPersistedKid(accountJid: string, serviceJid: string): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.removeItem(pushKidStorageKey(accountJid, serviceJid));
  } catch {
    // best-effort cleanup
  }
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
///
/// Exported so the test suite can pin the comparison semantics
/// (cleared-localStorage rotation path is the entire point of this
/// helper; covering it via the full `ensureBrowserSubscriptionWithCurrentKey`
/// would require stubbing every PushManager + SW surface).
export function subscriptionApplicationKeyMatches(
  subscription: { options: { applicationServerKey?: ArrayBuffer | ArrayBufferView | null } },
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
  const existing = ArrayBuffer.isView(existingKey)
    ? new Uint8Array(existingKey.buffer, existingKey.byteOffset, existingKey.byteLength)
    : new Uint8Array(existingKey);
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
