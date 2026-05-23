// XEP-0492 per-chat notification settings (#532), backed by the
// user's XEP-0402 PEP bookmarks. Account-wide entries only — no
// identity-specific (identity-category / identity-type) entries in
// this slice. Per-DM settings are deferred to #720 because the DM
// carrier for XEP-0492 is not yet decided.

import { ref, shallowRef, type Ref } from "vue";
import type { BrowserXmppClient, NotifyMode, UserBookmarkItem } from "@/lib/xmpp-client";

/** User-visible copy for one notification mode, keyed by the XEP wire
 * name so the radio control and the i18n surface map 1:1.
 */
export const NOTIFY_MODE_LABEL: Record<NotifyMode, string> = {
  always: "All messages",
  "on-mention": "Mentions only",
  never: "Muted",
};

/** Short description for each mode used as the popover menu helper text. */
export const NOTIFY_MODE_HINT: Record<NotifyMode, string> = {
  always: "Notify me for every new message in this chat.",
  "on-mention": "Notify me only when I'm mentioned.",
  never: "Don't notify me. Messages still appear in the chat.",
};

/** Conversation discriminator used by `resolveDefaultNotifyMode`.
 *
 * #532 v1 ships group/channel settings; per-DM is deferred to #720
 * because the DM carrier is undecided, but the resolver covers both
 * so the chat doesn't have to special-case once the DM slice lands.
 */
export type ConversationKind = "direct-chat" | "private-group" | "public-group";

/** XEP-0492 §3 last paragraph: "always" for direct chats and private
 * group chats, "on-mention" for public group chats.
 *
 * The chat layer currently lacks a public/private discriminator on
 * `ChannelSummary` (every MUC arrives without a `members_only` flag);
 * callers pass `"private-group"` for all groups until the public/
 * private slice lands. Documenting this conservative default here
 * keeps the rule auditable when the discriminator arrives.
 */
export function resolveDefaultNotifyMode(kind: ConversationKind): NotifyMode {
  switch (kind) {
    case "direct-chat":
    case "private-group":
      return "always";
    case "public-group":
      return "on-mention";
  }
}

/** Resolve the effective notification mode for a conversation given
 * its stored bookmark (or absence thereof) and conversation kind. */
export function effectiveNotifyMode(
  bookmark: UserBookmarkItem | undefined,
  kind: ConversationKind,
): NotifyMode {
  return bookmark?.notifyMode ?? resolveDefaultNotifyMode(kind);
}

interface NotifySettingsStore {
  /** Map keyed by bare room JID. Stored as a plain object so Vue's
   * reactivity tracks property assignments without depending on
   * Map proxy semantics. */
  readonly bookmarks: Ref<Record<string, UserBookmarkItem>>;
  /** True while the initial bookmark fetch is in flight; the UI uses
   * this to disable the mode picker until the cache is hydrated. */
  readonly hydrating: Ref<boolean>;
  /** Hydrate the cache by fetching every XEP-0402 bookmark from the
   * user's PEP node. Idempotent — safe to call on every reconnect. */
  hydrate(client: BrowserXmppClient): Promise<void>;
  /** Publish a new notification mode for one room and update the
   * cached bookmark on success. Returns `true` when the publish
   * round-trip succeeded; the caller does not need to refetch. */
  setMode(
    client: BrowserXmppClient,
    opts: { roomJid: string; mode: NotifyMode; name?: string },
  ): Promise<boolean>;
  /** Resolve the effective mode for `roomJid`. */
  getMode(roomJid: string, kind: ConversationKind): NotifyMode;
  /** Replace the cache wholesale — used by tests to set up fixtures
   * without going through the WASM client. */
  replaceAll(items: UserBookmarkItem[]): void;
}

/** Build a notification-settings store wired to `WaddleClient`. The
 * module-level singleton is exposed via `notifySettingsStore` so the
 * Vue components and the shell controller share state without prop
 * drilling. */
export function createNotifySettingsStore(): NotifySettingsStore {
  const bookmarks = shallowRef<Record<string, UserBookmarkItem>>({});
  const hydrating = ref<boolean>(false);

  function commit(items: UserBookmarkItem[]): void {
    const next: Record<string, UserBookmarkItem> = {};
    for (const item of items) next[item.jid] = item;
    bookmarks.value = next;
  }

  async function hydrate(client: BrowserXmppClient): Promise<void> {
    hydrating.value = true;
    try {
      const items = await client.fetchUserBookmarks();
      commit(items);
    } finally {
      hydrating.value = false;
    }
  }

  async function setMode(
    client: BrowserXmppClient,
    opts: { roomJid: string; mode: NotifyMode; name?: string },
  ): Promise<boolean> {
    const updated = await client.setRoomNotificationMode(opts);
    if (!updated) return false;
    bookmarks.value = { ...bookmarks.value, [updated.jid]: updated };
    return true;
  }

  function getMode(roomJid: string, kind: ConversationKind): NotifyMode {
    return effectiveNotifyMode(bookmarks.value[roomJid], kind);
  }

  function replaceAll(items: UserBookmarkItem[]): void {
    commit(items);
  }

  return { bookmarks, hydrating, hydrate, setMode, getMode, replaceAll };
}

/** Module-level singleton store. Use this from Vue components and
 * the chat shell. Tests construct fresh stores via
 * [[createNotifySettingsStore]]. */
export const notifySettingsStore: NotifySettingsStore = createNotifySettingsStore();
