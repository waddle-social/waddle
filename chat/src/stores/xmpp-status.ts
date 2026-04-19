import { atom } from "nanostores";
import type { XmppStatusSnapshot } from "@/lib/xmpp/types";

export const OFFLINE_SNAPSHOT: XmppStatusSnapshot = {
  state: "offline",
  detail: "Live room offline",
};

/**
 * Authoritative connection status shared across the component tree.
 *
 * Lives outside `useMessaging` so it survives `ChatApp` remounts on route
 * transitions (e.g. /settings -> back). The single writer is `XmppProvider`
 * (persisted via Astro `transition:persist`), which registers one
 * `setStatusHandler` on the `BrowserXmppClient` and mirrors every snapshot
 * into this atom. All readers use `useStore($xmppStatus)`.
 */
export const $xmppStatus = atom<XmppStatusSnapshot>(OFFLINE_SNAPSHOT);
