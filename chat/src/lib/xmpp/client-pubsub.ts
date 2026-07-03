/**
 * PubSub/PEP surface extracted from `BrowserXmppClient` (stage-2
 * decomposition of `client.ts`): XEP-0060 node subscribe/publish for
 * the community feed (XEP-0472), stories (XEP-0501) plus story
 * reactions/read-state, xCal community events, the PEP profile
 * (XEP-0107 mood / XEP-0108 activity / XEP-0118 tune), the private
 * status-preference node, and XEP-0402/XEP-0492 bookmark notification
 * settings.
 */
import type { StatusPreferenceWire } from "@/presence/status-preference";
import { barePeerJid } from "./jid";
import { escapeXml } from "./extension-commands/xml";
import {
  eventFromWasm,
  rruleToWasm,
  type CommunityEvent,
  type CommunityEventInput,
  type PartStat,
  type WasmVEvent,
} from "./event-types";
import {
  feedEntryFromWasm,
  type FeedEntry,
  type FeedPostInput,
  type WasmFeedEntry,
} from "./feed-types";
import type {
  ActivityPublication,
  GeneralActivity,
  MoodKind,
  MoodPublication,
  TunePublication,
  UserPepProfile,
} from "./pep-types";
import {
  storyReadsFromWasm,
  storyReadsToWasm,
  type StoryReads,
  type WasmStoryReads,
} from "./story-reads-types";
import {
  NS_PUBSUB_ATTACHMENTS,
  NS_PUBSUB_ATTACHMENTS_SUMMARY,
  STORIES_NODE,
  STORY_REACTIONS_SUMMARY_NODE,
  normalizeStoryReactions,
  storyAttachmentNode,
  storyFromWasm,
  type Story,
  type StoryPostInput,
  type StoryReactionItem,
  type StoryReactionSummary,
  type WasmStory,
} from "./story-types";
import type { WasmPepProfile } from "./wasm-types";

const NS_PUBSUB = "http://jabber.org/protocol/pubsub";

/** XEP-0492 fallback notification mode — kebab-case to match the wire
 * name produced by the WASM bridge. The chat UI presents these as
 * "All messages", "Mentions only", "Muted" respectively.
 */
export type NotifyMode = "always" | "on-mention" | "never";

/**
 * One XEP-0402 bookmark item surfaced from
 * `WaddleClient.fetchUserBookmarks` / `setRoomNotificationMode`.
 *
 * `notifyMode` is `null` when the bookmark exists but has no
 * `<notify/>` extension yet (a bookmark another client wrote, or a
 * Waddle bookmark we created before this slice). The chat resolves
 * `null` against the conversation-kind default per XEP-0492 §3 via
 * `resolveDefaultNotifyMode`.
 */
export interface UserBookmarkItem {
  jid: string;
  name: string | null;
  autojoin: boolean;
  notifyMode: NotifyMode | null;
  /** Waddle rich XEP-0357 push-summary opt-in, carried in the
   * XEP-0492 §2.3 `<advanced/>` block of this conversation's
   * `<notify/>` as `<rich-payload xmlns='urn:waddle:push:rich:0'/>`
   * (#719). `false` is the default — the minimal push payload. */
  richPayloadOptIn: boolean;
}

/** Typed outcome of [[BrowserXmppClient.setRoomNotificationMode]].
 *
 * `node-config-mismatch` separates the recoverable XEP-0060
 * `precondition-not-met` case (a pre-existing PEP node with a
 * different `access_model`) from generic transport / parser failures.
 * Round-8 XEP reviewer P2. */
export type SetRoomNotificationModeOutcome =
  | { kind: "ok"; item: UserBookmarkItem }
  | { kind: "node-config-mismatch" }
  | { kind: "error" };

/**
 * One per-DM notification entry surfaced from
 * `WaddleClient.fetchDmBookmarks` / `setDmNotificationMode`,
 * backed by the `urn:waddle:dm-bookmarks:0` PEP node (#720).
 *
 * Structurally identical to [[UserBookmarkItem]] minus the MUC-only
 * `name` / `autojoin` attributes — a DM has no room name or autojoin.
 * The chat store merges DM and MUC entries into one JID-keyed cache;
 * Waddle's DM (user) and MUC (service) JIDs are disjoint in practice,
 * and the server source-scopes its projection cleanup, so the unified
 * cache is safe.
 * `notifyMode` is `null` when the entry exists but carries no
 * XEP-0492 `<notify/>` mode yet; the chat resolves `null` against the
 * `direct-chat` §3 default (`always`) via `resolveDefaultNotifyMode`.
 */
export interface DmBookmarkItem {
  jid: string;
  notifyMode: NotifyMode | null;
  /** Waddle rich XEP-0357 push-summary opt-in for this DM (#719). */
  richPayloadOptIn: boolean;
}

/** Typed outcome of [[BrowserXmppClient.setDmNotificationMode]].
 *
 * Mirrors [[SetRoomNotificationModeOutcome]] plus the `removed`
 * variant: when a DM reverts to its XEP-0492 §3 default (`always`),
 * the `urn:waddle:dm-bookmarks:0` PEP item is retracted server-side,
 * so the chat drops that JID's cache entry rather than storing a
 * default-valued bookmark. */
export type SetDmNotificationModeResult =
  | { kind: "ok"; item: DmBookmarkItem }
  | { kind: "removed"; jid: string }
  | { kind: "node-config-mismatch" }
  | { kind: "error" };

export function parseStoryReactionItems(xml: string): StoryReactionItem[] {
  if (typeof DOMParser === "undefined") return [];
  const doc = new DOMParser().parseFromString(xml, "text/xml");
  const serializer = typeof XMLSerializer !== "undefined" ? new XMLSerializer() : null;
  return Array.from(doc.getElementsByTagName("item"))
    .filter((item) => item.namespaceURI === NS_PUBSUB)
    .map((item): StoryReactionItem | null => {
      const jid = item.getAttribute("id")?.trim();
      if (!jid) return null;
      const attachments = Array.from(item.children).find(
        (child) => child.localName === "attachments" && child.namespaceURI === NS_PUBSUB_ATTACHMENTS,
      );
      if (!attachments) return null;
      const reactions = Array.from(attachments.children).find(
        (child) => child.localName === "reactions" && child.namespaceURI === NS_PUBSUB_ATTACHMENTS,
      );
      const emojis = reactions
        ? Array.from(reactions.children)
            .filter((child) => child.localName === "reaction" && child.namespaceURI === NS_PUBSUB_ATTACHMENTS)
            .map((child) => child.textContent?.trim() ?? "")
        : [];
      const unknownChildrenXml = Array.from(attachments.children)
        .filter((child) => !(child.localName === "reactions" && child.namespaceURI === NS_PUBSUB_ATTACHMENTS))
        .map((child) => serializer?.serializeToString(child) ?? "")
        .filter(Boolean);
      return {
        jid,
        emojis: normalizeStoryReactions(emojis),
        unknownChildrenXml,
      };
    })
    .filter((item): item is StoryReactionItem => item !== null);
}

export function parseStoryReactionSummary(xml: string): StoryReactionSummary {
  const empty: StoryReactionSummary = { counts: {}, reactors: {}, mine: [] };
  if (typeof DOMParser === "undefined") return empty;
  const doc = new DOMParser().parseFromString(xml, "text/xml");
  const summary = Array.from(doc.getElementsByTagName("summary"))
    .find((element) => element.namespaceURI === NS_PUBSUB_ATTACHMENTS_SUMMARY);
  if (!summary) return empty;
  const reactions = Array.from(summary.children)
    .find((child) => child.localName === "reactions" && child.namespaceURI === NS_PUBSUB_ATTACHMENTS_SUMMARY);
  const counts: Record<string, number> = {};
  if (reactions) {
    for (const reaction of Array.from(reactions.children)) {
      if (reaction.localName !== "reaction" || reaction.namespaceURI !== NS_PUBSUB_ATTACHMENTS_SUMMARY) continue;
      const emoji = reaction.textContent?.trim() ?? "";
      if (!emoji) continue;
      const count = parsePositiveIntegerAttribute(reaction.getAttribute("count")) ?? 1;
      counts[emoji] = count;
    }
  }
  const noticed = Array.from(summary.children)
    .find((child) => child.localName === "noticed" && child.namespaceURI === NS_PUBSUB_ATTACHMENTS_SUMMARY);
  const noticedCount = noticed ? parsePositiveIntegerAttribute(noticed.getAttribute("count")) : undefined;
  return {
    counts,
    reactors: {},
    mine: [],
    ...(typeof noticedCount === "number" && Number.isFinite(noticedCount) ? { noticedCount } : {}),
  };
}

function parsePositiveIntegerAttribute(value: string | null): number | undefined {
  if (!value || !/^[1-9]\d*$/.test(value)) return undefined;
  const parsed = Number.parseInt(value, 10);
  return Number.isSafeInteger(parsed) ? parsed : undefined;
}

/**
 * Structural subset of the WASM client the pubsub module drives. The
 * generated bindings type these as `Promise<any>`; narrowing here
 * keeps every downstream value typed.
 */
export type PubsubWasmClient = {
  send_raw_iq?: (xml: string) => Promise<string>;
  fetch_user_bookmarks?: () => Promise<UserBookmarkItem[] | null>;
  set_room_notification_mode?: (options: {
    roomJid: string;
    mode: NotifyMode;
    name?: string;
    richPayloadOptIn?: boolean;
  }) => Promise<SetRoomNotificationModeOutcome>;
  fetch_dm_bookmarks?: () => Promise<DmBookmarkItem[] | null>;
  set_dm_notification_mode?: (options: {
    dmJid: string;
    mode: NotifyMode;
    richPayloadOptIn: boolean;
  }) => Promise<SetDmNotificationModeResult>;
  feed_items?: (communityJid: string, maxItems: number | null) => Promise<WasmFeedEntry[] | undefined>;
  feed_publish?: (communityJid: string, entry: unknown) => Promise<WasmFeedEntry | undefined>;
  stories_items?: (communityJid: string, maxItems: number | null) => Promise<WasmStory[] | undefined>;
  stories_publish?: (communityJid: string, input: unknown) => Promise<WasmStory | undefined>;
  story_reads_fetch?: () => Promise<WasmStoryReads | undefined>;
  story_reads_publish?: (input: unknown) => Promise<WasmStoryReads | undefined>;
  status_preference_publish?: (input: StatusPreferenceWire) => Promise<StatusPreferenceWire | null>;
  status_preference_fetch?: () => Promise<StatusPreferenceWire | null>;
  xcal_items?: (communityJid: string, maxItems: number | null) => Promise<WasmVEvent[] | undefined>;
  xcal_publish?: (communityJid: string, input: unknown) => Promise<WasmVEvent | undefined>;
  xcal_publish_item?: (communityJid: string, itemId: string, input: unknown) => Promise<WasmVEvent | undefined>;
  xcal_retract?: (communityJid: string, itemId: string) => Promise<unknown>;
  xcal_rsvp?: (communityJid: string, masterUid: string, selfLocalpart: string, selfJid: string, partstat: string) => Promise<unknown>;
  publish_mood?: (mood: unknown) => Promise<unknown>;
  retract_mood?: () => Promise<unknown>;
  publish_activity?: (activity: unknown) => Promise<unknown>;
  retract_activity?: () => Promise<unknown>;
  publish_tune?: (tune: TunePublication) => Promise<unknown>;
  retract_tune?: () => Promise<unknown>;
  fetch_user_pep_profile?: (jid: string) => Promise<WasmPepProfile | null>;
};

type PubsubManagerDeps = {
  /** The session's account JID (bare form used for pubsub item ids). */
  sessionJid: () => string;
  requireConnectedXmpp: () => Promise<PubsubWasmClient>;
};

export class PubsubManager {
  constructor(private readonly deps: PubsubManagerDeps) {}

  private selfBare(): string {
    return barePeerJid(this.deps.sessionJid());
  }

  async subscribeStoryReactionSummaries(communityJid: string): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (typeof xmpp.send_raw_iq !== "function") return;
    try {
      await xmpp.send_raw_iq(
        `<iq type="set" id="${crypto.randomUUID()}" to="${escapeXml(communityJid)}"><pubsub xmlns="${NS_PUBSUB}"><subscribe node="${escapeXml(STORY_REACTIONS_SUMMARY_NODE)}" jid="${escapeXml(this.selfBare())}"/></pubsub></iq>`,
      );
    } catch {
      /* best-effort */
    }
  }

  async subscribeStories(communityJid: string): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (typeof xmpp.send_raw_iq !== "function") return;
    try {
      await xmpp.send_raw_iq(
        `<iq type="set" id="${crypto.randomUUID()}" to="${escapeXml(communityJid)}"><pubsub xmlns="${NS_PUBSUB}"><subscribe node="${escapeXml(STORIES_NODE)}" jid="${escapeXml(this.selfBare())}"/></pubsub></iq>`,
      );
    } catch {
      /* best-effort */
    }
  }

  async fetchUserBookmarks(): Promise<UserBookmarkItem[]> {
    // Wrap the whole call — including `requireConnectedXmpp()` — so
    // a reconnect/teardown race (the client instance exists but the
    // session isn't ready) doesn't escape as an unhandled
    // rejection. `notifySettingsStore.hydrate` is dispatched with
    // `void` from the lifecycle handler, so a thrown rejection here
    // becomes an unhandled Promise rejection on flaky networks
    // exactly when hydrate fires. Round-14 PR review.
    try {
      const xmpp = await this.deps.requireConnectedXmpp();
      if (!xmpp.fetch_user_bookmarks) return [];
      const items = await xmpp.fetch_user_bookmarks();
      return items ?? [];
    } catch (error) {
      console.warn("[xmpp] XEP-0402 bookmark fetch failed:", error);
      return [];
    }
  }

  /**
   * Set the XEP-0492 fallback notification mode for one room.
   *
   * The WASM bridge is fetch-merge-publish: it preserves the rest of
   * the user's bookmark for that room (name, autojoin) as well as
   * foreign extensions / identity-scoped notify siblings that another
   * client may have written (XEP-0492 §3 first paragraph). On success,
   * resolves to the updated bookmark — the chat store should replace
   * its cached entry with this value rather than trusting the
   * requested mode in isolation.
   */
  async setRoomNotificationMode(opts: {
    roomJid: string;
    mode: "always" | "on-mention" | "never";
    name?: string;
    /** Waddle rich XEP-0357 push-summary opt-in (#719). When omitted,
     * the WASM bridge defaults it to `false` (minimal payload). */
    richPayloadOptIn?: boolean;
  }): Promise<SetRoomNotificationModeOutcome> {
    // Wrap the WHOLE call — including `requireConnectedXmpp()` — so
    // a reconnect/teardown race (the client instance exists but the
    // session isn't ready) doesn't escape as an exception and break
    // the typed-outcome contract documented at the call site.
    // Round-13 PR review.
    try {
      const xmpp = await this.deps.requireConnectedXmpp();
      if (!xmpp.set_room_notification_mode) return { kind: "error" };
      // WASM resolves with a typed tagged outcome — no stringly-typed
      // condition transport across the JS↔Rust boundary (round-13 PR
      // compliance #19). The specific RFC 6120 §8.3 condition stays
      // on the Rust side as a tracing diagnostic; chat-side
      // branching is on the `kind` discriminator only.
      const outcome = await xmpp.set_room_notification_mode({
        roomJid: opts.roomJid,
        mode: opts.mode,
        name: opts.name,
        richPayloadOptIn: opts.richPayloadOptIn,
      });
      if (outcome.kind === "error") {
        console.warn("[xmpp] XEP-0492 bookmark publish rejected");
      }
      return outcome;
    } catch (error) {
      // Reached for session-not-ready exceptions thrown from
      // requireConnectedXmpp, plus transport / serialization
      // failures. Stanza errors are surfaced via the typed outcome
      // above and never reach here.
      console.warn("[xmpp] XEP-0492 bookmark publish failed:", error);
      return { kind: "error" };
    }
  }

  /**
   * Fetch the user's per-DM XEP-0492 notification settings from the
   * `urn:waddle:dm-bookmarks:0` PEP node (#720).
   *
   * Returns an empty list when the user has not yet published any
   * per-DM override — the on-first-connect state — and the chat UI
   * resolves the `direct-chat` §3 default (`always`) via
   * [[resolveDefaultNotifyMode]]. `item-not-found` (the node has never
   * been created) collapses to an empty array on the WASM side.
   *
   * Wrapped like [[fetchUserBookmarks]] so a reconnect/teardown race
   * never escapes as an unhandled rejection out of the lifecycle
   * handler's `void`-dispatched hydrate.
   */
  async fetchDmBookmarks(): Promise<DmBookmarkItem[]> {
    try {
      const xmpp = await this.deps.requireConnectedXmpp();
      if (!xmpp.fetch_dm_bookmarks) return [];
      const items = await xmpp.fetch_dm_bookmarks();
      return items ?? [];
    } catch (error) {
      console.warn("[xmpp] DM notification-settings fetch failed:", error);
      return [];
    }
  }

  /**
   * Set the XEP-0492 fallback notification mode for one DM, persisted
   * to the `urn:waddle:dm-bookmarks:0` PEP node (#720).
   *
   * Resolves to a typed tagged outcome mirroring
   * [[setRoomNotificationMode]] plus a `removed` variant: a request
   * that reverts the DM to its §3 default (`always`) retracts the PEP
   * item server-side and resolves `{ kind: "removed", jid }` so the
   * chat store can drop the cache entry. A `dmJid` without a localpart
   * REJECTS on the WASM side; we map that — and any session-not-ready
   * / transport failure — to the typed `{ kind: "error" }` so the
   * UI banner contract holds and no exception escapes the call site.
   */
  async setDmNotificationMode(opts: {
    dmJid: string;
    mode: "always" | "on-mention" | "never";
    richPayloadOptIn: boolean;
  }): Promise<SetDmNotificationModeResult> {
    try {
      const xmpp = await this.deps.requireConnectedXmpp();
      if (!xmpp.set_dm_notification_mode) return { kind: "error" };
      const outcome = await xmpp.set_dm_notification_mode({
        dmJid: opts.dmJid,
        mode: opts.mode,
        richPayloadOptIn: opts.richPayloadOptIn,
      });
      if (outcome.kind === "error") {
        console.warn("[xmpp] DM notification-settings publish rejected");
      }
      return outcome;
    } catch (error) {
      // Reached for a malformed `dmJid` (no localpart → typed JS
      // reject), session-not-ready exceptions, and transport /
      // serialization failures. Stanza errors surface via the typed
      // outcome above and never reach here.
      console.warn("[xmpp] DM notification-settings publish failed:", error);
      return { kind: "error" };
    }
  }

  /**
   * Fetch the latest XEP-0472 social feed entries from the community
   * service. Returns entries newest-first as the server orders them.
   */
  async fetchFeed(communityJid: string, maxItems: number | undefined = undefined): Promise<FeedEntry[]> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const result = await xmpp.feed_items?.(communityJid, maxItems ?? null);
    return (result ?? []).map(feedEntryFromWasm);
  }

  /**
   * Publish a XEP-0472 entry to the community feed. Resolves with the
   * server-confirmed entry (with id + published) so callers can append
   * to local state without re-fetching.
   */
  async publishFeedPost(communityJid: string, post: FeedPostInput): Promise<FeedEntry> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const result = await xmpp.feed_publish?.(communityJid, {
      body: post.body,
      ...(post.title ? { title: post.title } : {}),
      ...(post.author ? { author: post.author } : {}),
      ...(post.link ? { link: post.link } : {}),
    });
    if (!result) {
      throw new Error("feed_publish returned no entry");
    }
    return feedEntryFromWasm(result);
  }

  /**
   * Fetch the latest XEP-0501 stories from the community stories
   * node. Returns ALL items (including expired); the chat filters
   * active vs expired locally.
   */
  async fetchStories(communityJid: string, maxItems: number | undefined = undefined): Promise<Story[]> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const result = await xmpp.stories_items?.(communityJid, maxItems ?? null);
    return (result ?? []).map(storyFromWasm);
  }

  /**
   * Fetch the current user's story read-state from their private PEP
   * node (XEP-0223 `urn:waddle:story:reads:0`). Read-state is
   * non-critical: any failure (no item yet, network error, spoofed
   * `from`) silently produces an empty result rather than surfacing
   * an error to the UI.
   */
  async fetchStoryReads(): Promise<StoryReads> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const result = await xmpp.story_reads_fetch?.();
    return storyReadsFromWasm(result);
  }

  /**
   * Publish the current user's story read-state. Overwrites the
   * single `current` item on every call.
   */
  async publishStoryReads(reads: StoryReads): Promise<StoryReads> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const result = await xmpp.story_reads_publish?.(storyReadsToWasm(reads));
    return storyReadsFromWasm(result);
  }

  /**
   * Publish the user's picked presence mode to their private
   * `urn:waddle:status-preference:0` PEP node (ADR-010 Phase 4), so it
   * syncs to their other devices. Reset publishes `mode='automatic'`
   * rather than retracting (the node never fans out retracts).
   */
  async publishStatusPreference(pref: StatusPreferenceWire): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.status_preference_publish?.(pref);
  }

  /**
   * Fetch the user's stored status preference from their own PEP node,
   * or `null` when nothing is stored. The preference is non-critical:
   * any failure resolves to `null` (the chat layer then keeps the
   * device's current mode).
   */
  async fetchStatusPreference(): Promise<StatusPreferenceWire | null> {
    const xmpp = await this.deps.requireConnectedXmpp();
    return (await xmpp.status_preference_fetch?.()) ?? null;
  }

  /**
   * Publish a XEP-0501 story. `mediaUrl` is required; `body` is
   * optional text content attached to that media.
   */
  async publishStory(communityJid: string, input: StoryPostInput): Promise<Story> {
    const mediaUrl = input.mediaUrl.trim();
    if (!mediaUrl) {
      throw new Error("Story mediaUrl is required");
    }
    const xmpp = await this.deps.requireConnectedXmpp();
    const result = await xmpp.stories_publish?.(communityJid, {
      ...(input.body ? { body: input.body } : {}),
      media_url: mediaUrl,
      ...(input.mediaType ? { media_type: input.mediaType } : {}),
      ...(input.author ? { author: input.author } : {}),
      ...(typeof input.expiryHours === "number" ? { expiry_hours: input.expiryHours } : {}),
    });
    if (!result) {
      throw new Error("stories_publish returned no story");
    }
    return storyFromWasm(result);
  }

  async fetchStoryReactions(communityJid: string, storyId: string): Promise<StoryReactionItem[]> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (typeof xmpp.send_raw_iq !== "function") return [];
    const node = storyAttachmentNode(communityJid, storyId);
    const responseXml = await xmpp.send_raw_iq(
      `<iq type="get" id="${crypto.randomUUID()}" to="${escapeXml(communityJid)}"><pubsub xmlns="${NS_PUBSUB}"><items node="${escapeXml(node)}"/></pubsub></iq>`,
    );
    return parseStoryReactionItems(responseXml);
  }

  async fetchMyStoryReactions(communityJid: string, storyId: string): Promise<StoryReactionItem | null> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (typeof xmpp.send_raw_iq !== "function") return null;
    const node = storyAttachmentNode(communityJid, storyId);
    const self = this.selfBare();
    const responseXml = await xmpp.send_raw_iq(
      `<iq type="get" id="${crypto.randomUUID()}" to="${escapeXml(communityJid)}"><pubsub xmlns="${NS_PUBSUB}"><items node="${escapeXml(node)}"><item id="${escapeXml(self)}"/></items></pubsub></iq>`,
    );
    return parseStoryReactionItems(responseXml)
      .find((item) => item.jid.toLowerCase() === self.toLowerCase()) ?? null;
  }

  async fetchStoryReactionSummary(communityJid: string, storyId: string): Promise<StoryReactionSummary> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (typeof xmpp.send_raw_iq !== "function") return { counts: {}, reactors: {}, mine: [] };
    const responseXml = await xmpp.send_raw_iq(
      `<iq type="get" id="${crypto.randomUUID()}" to="${escapeXml(communityJid)}"><pubsub xmlns="${NS_PUBSUB}"><items node="${escapeXml(STORY_REACTIONS_SUMMARY_NODE)}"><item id="${escapeXml(storyId)}"/></items></pubsub></iq>`,
    );
    return parseStoryReactionSummary(responseXml);
  }

  async publishStoryReactions(
    communityJid: string,
    storyId: string,
    emojis: readonly string[],
    unknownChildrenXml: readonly string[] = [],
  ): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (typeof xmpp.send_raw_iq !== "function") throw new Error("XMPP session is not ready");
    const node = storyAttachmentNode(communityJid, storyId);
    const normalized = normalizeStoryReactions(emojis);
    if (normalized.length === 0) {
      await this.retractStoryReactions(communityJid, storyId);
      return;
    }
    const reactionsXml = `<reactions>${normalized.map((emoji) => `<reaction>${escapeXml(emoji)}</reaction>`).join("")}</reactions>`;
    const preservedXml = unknownChildrenXml.join("");
    await xmpp.send_raw_iq(
      `<iq type="set" id="${crypto.randomUUID()}" to="${escapeXml(communityJid)}"><pubsub xmlns="${NS_PUBSUB}"><publish node="${escapeXml(node)}"><item id="${escapeXml(this.selfBare())}"><attachments xmlns="${NS_PUBSUB_ATTACHMENTS}">${reactionsXml}${preservedXml}</attachments></item></publish></pubsub></iq>`,
    );
  }

  async retractStoryReactions(communityJid: string, storyId: string): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (typeof xmpp.send_raw_iq !== "function") throw new Error("XMPP session is not ready");
    const node = storyAttachmentNode(communityJid, storyId);
    await xmpp.send_raw_iq(
      `<iq type="set" id="${crypto.randomUUID()}" to="${escapeXml(communityJid)}"><pubsub xmlns="${NS_PUBSUB}"><retract node="${escapeXml(node)}"><item id="${escapeXml(this.selfBare())}"/></retract></pubsub></iq>`,
    );
  }

  /**
   * Fetch xCal community events. Returns ALL items (past + upcoming);
   * the chat sorts upcoming-first.
   */
  async fetchCommunityEvents(communityJid: string, maxItems: number | undefined = undefined): Promise<CommunityEvent[]> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const result = await xmpp.xcal_items?.(communityJid, maxItems ?? null);
    return (result ?? []).map(eventFromWasm);
  }

  /**
   * Publish an xCal community event. SUMMARY is required; DTSTART
   * is required for the event to be useful on a timeline.
   */
  async publishCommunityEvent(communityJid: string, input: CommunityEventInput): Promise<CommunityEvent> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const result = await xmpp.xcal_publish?.(communityJid, {
      summary: input.summary,
      ...(input.description ? { description: input.description } : {}),
      ...(input.location ? { location: input.location } : {}),
      ...(input.organizer ? { organizer: input.organizer } : {}),
      ...(input.dtstart ? { dtstart: input.dtstart } : {}),
      ...(input.dtend ? { dtend: input.dtend } : {}),
      ...(input.rrule ? { rrule: rruleToWasm(input.rrule) } : {}),
    });
    if (!result) {
      throw new Error("xcal_publish returned no event");
    }
    return eventFromWasm(result);
  }

  /**
   * Replace an existing community event item with new master values.
   * Uses `xcal_publish_item` which atomically overwrites the item at
   * `itemId`, preserving any sibling RSVPs (those live on their own
   * `-rsvp-*` item ids).
   */
  async updateCommunityEvent(
    communityJid: string,
    itemId: string,
    input: CommunityEventInput,
  ): Promise<CommunityEvent> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const exdates = input.exdates ?? [];
    const result = await xmpp.xcal_publish_item?.(communityJid, itemId, {
      master: {
        summary: input.summary,
        ...(input.description ? { description: input.description } : {}),
        ...(input.location ? { location: input.location } : {}),
        ...(input.organizer ? { organizer: input.organizer } : {}),
        ...(input.dtstart ? { dtstart: input.dtstart } : {}),
        ...(input.dtend ? { dtend: input.dtend } : {}),
        ...(input.rrule ? { rrule: rruleToWasm(input.rrule) } : {}),
      },
      overrides: [],
      exdates,
    });
    if (!result) {
      throw new Error("xcal_publish_item returned no event");
    }
    return eventFromWasm(result);
  }

  /**
   * Retract (delete) a community event item. Removes the master plus
   * any per-instance overrides in one shot; sibling RSVP items live
   * under their own ids and stay behind until separately retracted.
   */
  async retractCommunityEvent(communityJid: string, itemId: string): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.xcal_retract?.(communityJid, itemId);
  }

  /**
   * Publish (or update) this session's RSVP for a calendar event.
   * The server bridges the call to a sibling pubsub item; the chat
   * folds it back into the master event on the next refresh.
   */
  async rsvpCommunityEvent(
    communityJid: string,
    masterUid: string,
    selfLocalpart: string,
    selfBareJid: string,
    partstat: PartStat,
  ): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.xcal_rsvp?.(communityJid, masterUid, selfLocalpart, selfBareJid, partstat);
  }

  async publishMood(mood: MoodPublication): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.publish_mood?.({ kind: mood.kind, ...(mood.text ? { text: mood.text } : {}) });
  }

  async retractMood(): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.retract_mood?.();
  }

  async publishActivity(activity: ActivityPublication): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.publish_activity?.({ general: activity.general, ...(activity.specific ? { specific: activity.specific } : {}), ...(activity.text ? { text: activity.text } : {}) });
  }

  async retractActivity(): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.retract_activity?.();
  }

  async publishTune(tune: TunePublication): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.publish_tune?.(tune);
  }

  async retractTune(): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.retract_tune?.();
  }

  async fetchUserPepProfile(jid: string): Promise<UserPepProfile> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const profile = await xmpp.fetch_user_pep_profile?.(jid);
    // Wire `kind` / `general` arrive as plain strings from the wasm
    // bridge; the server publishes XEP-0107 / XEP-0108 vocabulary, so
    // they are asserted (not re-validated) against the chat unions.
    return { mood: profile?.mood ? { kind: profile.mood.kind as MoodKind, ...(profile.mood.text ? { text: profile.mood.text } : {}) } : null, activity: profile?.activity ? { general: profile.activity.general as GeneralActivity, ...(profile.activity.specific ? { specific: profile.activity.specific } : {}), ...(profile.activity.text ? { text: profile.activity.text } : {}) } : null, tune: profile?.tune ? { ...profile.tune } : null };
  }
}
