// XEP-0492 per-chat notification settings (#532), backed by the
// user's XEP-0402 PEP bookmarks. Account-wide entries only — no
// identity-specific (identity-category / identity-type) entries in
// this slice. Per-DM settings are deferred to #720 because the DM
// carrier for XEP-0492 is not yet decided.
//
// Cross-client coherence: we do NOT yet subscribe to PEP `+notify`
// headlines on `urn:xmpp:bookmarks:1` (XEP-0163 §4.4). The chat
// re-fetches on every fresh session-ready via `hydrate(client)`,
// so a setting changed in another tab reaches this tab on the next
// reconnect. Wiring the conformant headline route is deferred to
// a follow-up slice — it adds a new WASM event pipeline that's
// scoped out of #532.

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

/** Compute the next focus index for a WAI-ARIA radio menu given the
 * current focused index and a navigation step (+1 for ArrowDown / Right,
 * -1 for ArrowUp / Left). Wraps around. When no item is currently
 * focused (`current === -1`), the next/previous wraps cleanly to the
 * first / last item.
 *
 * Caller MUST pass `total > 0` — there is no valid menu navigation
 * on an empty menu. Extracted from `NotifyModeButton.vue` so the
 * index math can be unit-tested without a DOM (round-7 UX reviewer P2). */
export function nextMenuIndex(current: number, step: 1 | -1, total: number): number {
  // When nothing is focused, a "down" goes to first (0), "up" to last.
  const base = current < 0 ? (step === 1 ? -1 : 0) : current;
  return ((base + step) % total + total) % total;
}

export interface NotifySettingsStore {
  /** Map keyed by bare room JID. Stored as a plain object so Vue's
   * reactivity tracks property assignments without depending on
   * Map proxy semantics. */
  readonly bookmarks: Ref<Record<string, UserBookmarkItem>>;
  /** True while the initial bookmark fetch is in flight; the UI uses
   * this to disable the mode picker until the cache is hydrated. */
  readonly hydrating: Ref<boolean>;
  /** Hydrate the cache by fetching every XEP-0402 bookmark from the
   * user's PEP node. Re-entrancy guarded — concurrent calls
   * (`onConnectionReady` + lifecycle handler firing together)
   * collapse into one in-flight fetch. */
  hydrate(client: BrowserXmppClient): Promise<void>;
  /** Publish a new notification mode for one room and update the
   * cached bookmark on success. Returns `"ok"` when the publish
   * round-trip succeeded, `"node-config-mismatch"` when the server
   * rejected with `precondition-not-met` (typically a pre-existing
   * XEP-0223-style node with `access_model=open`), `"error"` for
   * any other failure. */
  setMode(
    client: BrowserXmppClient,
    opts: { roomJid: string; mode: NotifyMode; name?: string },
  ): Promise<SetModeResult>;
  /** Resolve the effective mode for `roomJid`. */
  getMode(roomJid: string, kind: ConversationKind): NotifyMode;
  /** Resolve the Waddle rich XEP-0357 push-summary opt-in for
   * `roomJid` from the cached bookmark. Absence — or an un-opted
   * bookmark — is `false` (the minimal push payload). #719. */
  getRichPayloadOptIn(roomJid: string): boolean;
  /** Publish a new rich-payload opt-in for one conversation while
   * preserving its current notification mode. The opt-in and the
   * `<notify>` mode share one XEP-0402 bookmark publish (the opt-in
   * lives in the `<advanced>` of the fallback setting), so this
   * resolves the conversation's effective mode (via [[getMode]] with
   * `kind`) and republishes both. Mirrors [[setMode]]'s typed
   * outcome. #719. */
  setRichPayloadOptIn(
    client: BrowserXmppClient,
    opts: { roomJid: string; optIn: boolean; kind: ConversationKind; name?: string },
  ): Promise<SetModeResult>;
  /** Replace the cache wholesale — used by tests to set up fixtures
   * without going through the WASM client. */
  replaceAll(items: UserBookmarkItem[]): void;
  /** Drop every cached bookmark and the in-flight indicator. Called
   * from `handleLogout` so a subsequent sign-in does not leak the
   * previous account's notification settings into UI reads while the
   * fresh `hydrate` is still pending. Round-8 UX reviewer P1. */
  reset(): void;
}

/** Typed outcome of [[NotifySettingsStore.setMode]]. The
 * `node-config-mismatch` variant lets the UI surface a specific
 * "this room's bookmark node was created by an older client; ask
 * an admin to delete it" hint instead of a generic failure. */
type SetModeResult = "ok" | "node-config-mismatch" | "error";

/** Build a notification-settings store wired to `WaddleClient`.
 *
 * Construct exactly one instance per chat-app-controller lifetime
 * (see `useChatAppController`) and thread it through the component
 * tree as a prop. A previous version exposed a module-level
 * singleton; that was dropped per the PR-review compliance note
 * that bans implicit shared state across unrelated consumers. */
export function createNotifySettingsStore(): NotifySettingsStore {
  const bookmarks = shallowRef<Record<string, UserBookmarkItem>>({});
  const hydrating = ref<boolean>(false);
  // Generation token — incremented on every `reset()`. In-flight
  // hydrate calls capture the generation at fetch-start; if the
  // generation changes (logout / account-switch) before the fetch
  // resolves, the commit is discarded. Without this, a stale
  // hydrate from the previous account would write its bookmarks
  // back into the cleared cache after the user signed into a
  // different account — round-9 reviewer P1.
  let generation = 0;

  function commit(items: UserBookmarkItem[]): void {
    const next: Record<string, UserBookmarkItem> = {};
    for (const item of items) next[item.jid] = item;
    bookmarks.value = next;
  }

  async function hydrate(client: BrowserXmppClient): Promise<void> {
    // Re-entrancy guard — round-8 UX reviewer P2. `onConnectionReady`
    // and the session-lifecycle handler both fire `hydrate` on first
    // connect; without this guard the two calls race and produce a
    // hydrating.value true → false → true → false flicker that
    // briefly enables the popover trigger between fetches.
    if (hydrating.value) return;
    // Do NOT clear the cache before the fetch resolves — clearing it
    // would make `effectiveNotifyMode` snap back to the §3 default on
    // every reconnect tick, producing a visible icon flicker for any
    // chat that has a non-default mode stored. Reviewer P2 round 7.
    // Commit happens on success only.
    const myGen = generation;
    hydrating.value = true;
    try {
      // Defence-in-depth — `client.fetchUserBookmarks` is supposed
      // to always resolve to a (possibly empty) array, but the
      // session-lifecycle handler invokes hydrate with `void`, so
      // any thrown rejection from a lower layer would surface as
      // an unhandled Promise rejection on flaky networks. Catch
      // and log, then proceed without committing. Round-14 PR
      // review.
      let items: UserBookmarkItem[];
      try {
        items = await client.fetchUserBookmarks();
      } catch (error) {
        console.warn("[notify-settings] fetchUserBookmarks threw:", error);
        return;
      }
      if (myGen !== generation) {
        // reset() fired while we were fetching — the user logged
        // out (or switched accounts) and our results no longer
        // belong in the cache. Drop them on the floor.
        return;
      }
      commit(items);
    } finally {
      // Only clear the in-flight flag if we still own it. If a
      // reset() ran during our fetch it has already cleared the
      // flag so a fresh sign-in hydrate can start.
      if (myGen === generation) {
        hydrating.value = false;
      }
    }
  }

  // Shared fetch-merge-publish path for both the notify mode and the
  // rich-payload opt-in — they live in the same XEP-0402 bookmark
  // `<notify>` element and so travel in one publish. #719.
  async function publish(
    client: BrowserXmppClient,
    opts: { roomJid: string; mode: NotifyMode; richPayloadOptIn: boolean; name?: string },
  ): Promise<SetModeResult> {
    // Capture the generation at call-start. If `reset()` fires
    // during the await (the user signed out / switched accounts),
    // the commit below would otherwise write the prior account's
    // bookmark into the new account's cache — round-12 reviewer P1.
    const myGen = generation;
    // Defence-in-depth: `client.setRoomNotificationMode` is supposed
    // to always resolve with a typed outcome, but if a lower layer
    // ever regresses to throwing (e.g. an unwrapped
    // requireConnectedXmpp() rejection), map the exception to the
    // same typed `"error"` result so the UI banner still fires.
    // Round-13 PR review.
    let outcome: Awaited<ReturnType<typeof client.setRoomNotificationMode>>;
    try {
      outcome = await client.setRoomNotificationMode(opts);
    } catch (error) {
      console.warn("[notify-settings] setRoomNotificationMode threw:", error);
      return "error";
    }
    if (myGen !== generation) {
      // Stale publish from a prior session; drop the commit. The
      // typed `kind` is still returned so the UI can clean up
      // its own state, but the caller is expected to recognize
      // that the surrounding session has been torn down.
      if (outcome.kind === "ok") return "ok";
      if (outcome.kind === "node-config-mismatch") return "node-config-mismatch";
      return "error";
    }
    if (outcome.kind === "ok") {
      bookmarks.value = { ...bookmarks.value, [outcome.item.jid]: outcome.item };
      return "ok";
    }
    if (outcome.kind === "node-config-mismatch") return "node-config-mismatch";
    return "error";
  }

  async function setMode(
    client: BrowserXmppClient,
    opts: { roomJid: string; mode: NotifyMode; name?: string },
  ): Promise<SetModeResult> {
    // Changing the mode preserves the conversation's current rich-
    // payload opt-in: both settings share one bookmark publish, so
    // re-send the cached opt-in rather than letting the WASM bridge
    // default it back to false and silently undo the user's choice.
    // #719.
    return publish(client, {
      roomJid: opts.roomJid,
      mode: opts.mode,
      name: opts.name,
      richPayloadOptIn: getRichPayloadOptIn(opts.roomJid),
    });
  }

  async function setRichPayloadOptIn(
    client: BrowserXmppClient,
    opts: { roomJid: string; optIn: boolean; kind: ConversationKind; name?: string },
  ): Promise<SetModeResult> {
    // The opt-in nests in the `<advanced>` of the fallback `<notify>`
    // setting, so flipping it republishes the conversation's current
    // effective mode unchanged (XEP-0492 §3 default when no bookmark
    // covers it yet). #719.
    return publish(client, {
      roomJid: opts.roomJid,
      mode: getMode(opts.roomJid, opts.kind),
      name: opts.name,
      richPayloadOptIn: opts.optIn,
    });
  }

  function getMode(roomJid: string, kind: ConversationKind): NotifyMode {
    return effectiveNotifyMode(bookmarks.value[roomJid], kind);
  }

  function getRichPayloadOptIn(roomJid: string): boolean {
    return bookmarks.value[roomJid]?.richPayloadOptIn ?? false;
  }

  function replaceAll(items: UserBookmarkItem[]): void {
    commit(items);
  }

  function reset(): void {
    generation += 1;
    bookmarks.value = {};
    hydrating.value = false;
  }

  return {
    bookmarks,
    hydrating,
    hydrate,
    setMode,
    setRichPayloadOptIn,
    getMode,
    getRichPayloadOptIn,
    replaceAll,
    reset,
  };
}
