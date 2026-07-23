/**
 * MAM (XEP-0313) paging extracted from `BrowserXmppClient` (stage-2
 * decomposition of `client.ts`): history fetch/paging for rooms and
 * DMs, thread pages, archive search, DM call-activity hydration, and
 * the reconnect catch-up pagination that replays the archive gap after
 * a resume failure. `ReconnectCatchup` (cursor store) stays a separate
 * module; this owns the paging loops that read and advance it.
 */
import {
  DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS,
  applyDmCallEvent,
} from "@/lib/calls/dm-call-activity";
import { buildDmCallOutcomeAnchor, type DmCallOutcomeAnchor } from "@/lib/calls/dm-call-anchor";
import { compareTimelineTimestamps } from "../timeline-timestamps";
import { bareJidKey, barePeerJid, fullJidIdentityKey, jidDomain, resourceOf } from "./jid";
import type { ClientEvents, TypedEventBus } from "./client-events";
import { classifyMamError, isMamCursorNotFound } from "./mam";
import type { DmConversationScope, ReconnectCatchup, ReconnectCatchupEntry } from "./reconnect-catchup";
import type {
  LiveDmMessage,
  LiveRoomMessage,
  MamHistoryPage,
  MamPageParam,
  MamThreadPageParam,
  MessageSearchResult,
  RoomActivityEvent,
  XmppErrorEvent,
} from "./types";
import { dmMessageFromArchived, roomMessageFromArchived, stanzaIdAuthorityKey } from "./wasm-message-codecs";
import type { WasmArchivedMessage, WasmMamPage, WasmMessage } from "./wasm-types";

const DM_CALL_ACTIVITY_PAGE_SIZE = 100;
const DM_CALL_ACTIVITY_MAX_PAGES = 50;

// #1221: cap reconnect catch-up paging per conversation (≤500 msgs at
// 100/page). Unbounded paging of a large room archive on every fresh
// reconnect overflowed the SM queue during the prod join storm. On
// exhaustion the loop throws into the per-conversation `catchupFailure`
// fallback (bounded wholesale reload); older history lazy-loads via the
// normal MAM pagination.
const RECONNECT_CATCHUP_MAX_PAGES_PER_CONVERSATION = 5;

export type DmCallActivityHydrationOptions = { since?: string; pageSize?: number; maxPages?: number };

type CatchupRunStats = { pages: number; pageFailures: number; messages: number };
type CatchupOutcome = "completed" | "aborted" | "failed";

/**
 * #1267 item 4: typed marker for catch-up page-budget exhaustion. Unlike
 * a transient IQ failure, budget exhaustion means messages beyond the cap
 * were genuinely NOT replayed — a real archive gap — so the failure
 * handler must trigger the wholesale-reload fallback even on a resumed
 * lifecycle, where transient failures are otherwise ignored (the resumed
 * stream itself is gap-free).
 */
class CatchupPageBudgetExceededError extends Error {}

type ArchivedDmCallOutcome = {
  anchor: DmCallOutcomeAnchor;
  terminalMessage: WasmArchivedMessage;
};

export function isRoomActivityMessage(message: LiveRoomMessage): boolean {
  return !!message.body && !message.replacesId && !message.retractsId;
}

export function roomActivityEventFromMessage(message: LiveRoomMessage): RoomActivityEvent {
  const activity: RoomActivityEvent = { roomJid: message.roomJid, nick: message.nick, body: message.body };
  if (message.stanzaId) activity.stanzaId = message.stanzaId;
  if (message.mentions) activity.mentions = message.mentions;
  if (message.broadcastMention) activity.broadcastMention = message.broadcastMention;
  if (message.createdAtSource === "archive") activity.fromArchive = true;
  return activity;
}

function isMamPageComplete(page: WasmMamPage | null | undefined): boolean {
  const compat = page as (WasmMamPage & { complete?: boolean }) | null | undefined;
  return !!(compat?.is_complete ?? compat?.complete);
}

function pageLastArchiveId(page: WasmMamPage | null | undefined): string | undefined {
  const compat = page as (WasmMamPage & { lastArchiveId?: string }) | null | undefined;
  return compat?.last_id ?? compat?.lastArchiveId;
}

function pageFirstArchiveId(page: WasmMamPage | null | undefined): string | undefined {
  const compat = page as (WasmMamPage & { firstArchiveId?: string }) | null | undefined;
  return compat?.first_id ?? compat?.firstArchiveId;
}

function pageContainsArchiveId(page: WasmMamPage | null | undefined, archiveId: string): boolean {
  return (page?.messages ?? []).some((message) => message.mam_id === archiveId);
}

function compareTimestamps(left: string, right: string): number {
  return compareTimelineTimestamps(left, right);
}

function pageCrossesSince(page: WasmMamPage | null | undefined, since: string): boolean {
  return (page?.messages ?? []).some((message) => typeof message.timestamp === "string" && compareTimestamps(message.timestamp, since) < 0);
}

function messageSeenIds(message: Pick<LiveDmMessage | LiveRoomMessage, "id" | "wireIds">): string[] {
  return Array.from(new Set([message.id, ...(message.wireIds ?? [])].filter(Boolean)));
}

/**
 * Wire ids of a raw stanza usable for catch-up dedupe. XEP-0359
 * §Security Considerations (#1267 item 2, #466 residual): a stanza-id is
 * only trustworthy when its `by` matches the archiving authority for
 * the conversation (the room bare JID for MUC, the user's server /
 * account for DMs) — an unverified sender-controlled id colliding with
 * a real archive id would make catch-up skip a legitimate message.
 */
export function rawMessageSeenIds(
  message: WasmMessage,
  stanzaIdAuthorities: ReadonlyArray<string>,
): string[] {
  // `stanzaIdAuthorityKey` (shared with the decode path) case-folds the
  // bare and rejects resource-carrying `by` values — the seen-id path
  // and the row-identity path MUST accept exactly the same ids, or a
  // mixed-case authority is recorded as seen while the row omits it
  // (duplicate on reconnect).
  const authorities = new Set(
    stanzaIdAuthorities.map(stanzaIdAuthorityKey).filter((value): value is string => !!value),
  );
  const verifiedStanzaIds = [
    ...(message.stanza_ids ?? []),
    ...(message.stanza_id && message.stanza_id_by
      ? [{ id: message.stanza_id, by: message.stanza_id_by }]
      : []),
  ]
    .filter((stanzaId) => {
      const key = stanzaIdAuthorityKey(stanzaId.by);
      return !!stanzaId.id && !!key && authorities.has(key);
    })
    .map((stanzaId) => stanzaId.id);
  return Array.from(new Set([
    message.id,
    message.origin_id,
    ...verifiedStanzaIds,
  ].filter((value): value is string => !!value)));
}

function shouldSkipCatchupMessage(
  message: Pick<LiveDmMessage | LiveRoomMessage, "createdAt" | "id" | "wireIds">,
  since?: string,
  seenIds?: ReadonlyArray<string>,
): boolean {
  const seen = new Set(seenIds ?? []);
  if (seen.size > 0 && messageSeenIds(message).some((id) => seen.has(id))) return true;
  if (!since) return false;
  const order = compareTimestamps(message.createdAt, since);
  if (order < 0) return true;
  if (order > 0) return false;
  return false;
}

function shouldSkipRawCatchupMessage(
  message: WasmMessage,
  stanzaIdAuthorities: ReadonlyArray<string>,
  since?: string,
  seenIds?: ReadonlyArray<string>,
): boolean {
  const seen = new Set(seenIds ?? []);
  if (seen.size > 0 && rawMessageSeenIds(message, stanzaIdAuthorities).some((id) => seen.has(id))) return true;
  if (!since || !message.timestamp) return false;
  return compareTimestamps(message.timestamp, since) < 0;
}

async function collectRecentMamPages(
  fetchPage: (max: number, pageParam: MamPageParam) => Promise<WasmMamPage | null | undefined>,
  options: DmCallActivityHydrationOptions,
  shouldContinue: () => boolean,
): Promise<WasmMamPage[]> {
  const since = options.since ?? new Date(Date.now() - DM_CALL_ACTIVITY_ACTIVE_WINDOW_MS).toISOString();
  const pageSize = options.pageSize ?? DM_CALL_ACTIVITY_PAGE_SIZE;
  const maxPages = options.maxPages ?? DM_CALL_ACTIVITY_MAX_PAGES;
  const pages: WasmMamPage[] = [];
  let pageParam: MamPageParam = { type: "latest", start: since };
  const seenBefore = new Set<string>();
  for (let pageIndex = 0; pageIndex < maxPages; pageIndex += 1) {
    if (!shouldContinue()) return [];
    const page = await fetchPage(pageSize, pageParam);
    if (!shouldContinue()) return [];
    if (!page) return pages;
    pages.push(page);
    if (isMamPageComplete(page) || pageCrossesSince(page, since)) return pages;
    const firstArchiveId = pageFirstArchiveId(page);
    if (!firstArchiveId || seenBefore.has(firstArchiveId)) return pages;
    seenBefore.add(firstArchiveId);
    pageParam = { type: "before", before: firstArchiveId, start: since };
  }
  return pages;
}

/**
 * Structural subset of the WASM client the MAM pager drives. The
 * generated bindings type these as `Promise<any>`; narrowing here keeps
 * every downstream value typed.
 */
export type MamWasmClient = {
  fetch_room_history_page?: (roomJid: string, max: number, pageParam: MamPageParam) => Promise<WasmMamPage | null | undefined>;
  fetch_room_history_by_thread?: (roomJid: string, threadId: string, max: number, beforeId?: string | null) => Promise<WasmMamPage | null | undefined>;
  fetch_dm_history_page?: (peerJid: string, max: number, pageParam: MamPageParam) => Promise<WasmMamPage | null | undefined>;
  fetch_dm_history_by_thread?: (peerJid: string, threadId: string, max: number, beforeId?: string | null) => Promise<WasmMamPage | null | undefined>;
  search_room_history?: (roomJid: string, query: string, max: number) => Promise<WasmMamPage | null | undefined>;
  search_dm_history?: (peerJid: string, query: string, max: number) => Promise<WasmMamPage | null | undefined>;
  fetch_personal_history_page?: (max: number, pageParam: MamPageParam) => Promise<WasmMamPage | null | undefined>;
};

type MamPagerDeps = {
  /** The session's account JID (bare). */
  sessionJid: () => string;
  /** Full JID (`bare/resource`) — the call layer stamps it on Jingle events. */
  fullJid: () => string;
  trustedMediaOrigin: () => string | null;
  /** Focused room — messages for other rooms surface as `activity`. */
  currentRoom: () => string | null;
  /** Shared catch-up cursor store (also advanced by live dispatch in the client). */
  catchup: ReconnectCatchup;
  events: TypedEventBus<ClientEvents>;
  emitError: (event: XmppErrorEvent) => void;
  requireConnectedXmpp: () => Promise<MamWasmClient>;
  /**
   * Connect + switch/join and return the exact validated room generation.
   *
   * A public room switch may resolve when a newer navigation supersedes it;
   * room-scoped IQ callers must therefore receive the post-switch handle and
   * room JID from the same readiness check instead of reacquiring a merely
   * connected session afterward.
   */
  ensureRoomReady: (
    spaceId: string,
    channelId: string,
  ) => Promise<{ xmpp: MamWasmClient; roomJid: string }>;
  roomJidForChannel: (channelId: string) => string;
  /** Identity + liveness gate for a specific WASM handle mid-pagination. */
  isCurrentConnected: (xmpp: MamWasmClient, sessionJid: string) => boolean;
  /**
   * XEP-0045 §7.5 (#1256): classify a raw DM-path stanza as a MUC private
   * message (counterpart is `room@service/nick` for a known room) so
   * archived/catch-up re-emissions key by the occupant JID exactly like
   * the live path. Optional so bare unit-test pagers keep working.
   */
  classifyMucPm?: (message: WasmMessage) => { occupantJid: string; nick: string } | undefined;
  /** Session-authoritative occupant scope derived from the configured MUC service. */
  isMucPmPeer: (peerJid: string) => boolean;
};

export class MamPager {
  constructor(private readonly deps: MamPagerDeps) {}

  private isMucPmPeer(peerJid: string, scope?: DmConversationScope): boolean {
    return scope === "muc-occupant" || this.deps.isMucPmPeer(peerJid);
  }

  private selfBare(): string {
    return barePeerJid(this.deps.sessionJid());
  }

  /** XEP-0359 archiving authorities for a DM: the user's own account
   * bare JID (what the Waddle server stamps) plus its server domain —
   * the SAME set the DM decode path (`assignedStanzaIdBy`) adopts as
   * the row identity/wireIds, so catch-up seen-ids can never diverge
   * from the ids the timeline rows carry (a divergence would re-emit
   * or strand rows). Both authorities are safe to trust: the server
   * strips sender-spoofed `<stanza-id/>` elements claiming either its
   * account-bare OR its domain authority on the inbound DM path
   * (XEP-0359 §5; #1275, fixed alongside this in PR #1273 —
   * `protocol/handlers/canonicalize.rs`). */
  private dmStanzaIdAuthorities(): string[] {
    const selfBare = this.selfBare();
    return [selfBare, jidDomain(selfBare)];
  }

  private roomPageToMessages(page: WasmMamPage): MamHistoryPage<LiveRoomMessage> {
    return { messages: page.messages.map((message) => roomMessageFromArchived(message, { trustedMediaOrigin: this.deps.trustedMediaOrigin() })).filter((message): message is LiveRoomMessage => !!message), ...(page.first_id ? { firstArchiveId: page.first_id } : {}), ...(page.last_id ? { lastArchiveId: page.last_id } : {}), complete: page.is_complete };
  }

  private dmPageToMessages(
    page: WasmMamPage,
    options: { applyCallEvents?: boolean; peerJid?: string; dmScope?: DmConversationScope } = {},
  ): MamHistoryPage<LiveDmMessage> {
    const selfBare = this.selfBare();
    const outcomeAnchors = options.applyCallEvents === false || this.isMucPmPeer(options.peerJid ?? "", options.dmScope)
      ? []
      : this.applyDmCallEventsFromMamPage(page, selfBare, {
          publishOutcome: false,
          ...(options.peerJid ? { peerJid: options.peerJid } : {}),
          ...(options.dmScope ? { dmScope: options.dmScope } : {}),
        });
    const requestedPeerJid = options.peerJid;
    const rawMessages = requestedPeerJid
      ? page.messages.filter((message) => this.rawDmMessageMatchesPeer(message, requestedPeerJid, options.dmScope))
      : page.messages;
    const messages = rawMessages
      .map((message) => {
        const converted = dmMessageFromArchived(message, selfBare, { trustedMediaOrigin: this.deps.trustedMediaOrigin() });
        return converted ? this.applyMucPmClassification(message, converted, options.peerJid, options.dmScope) : null;
      })
      .filter((message): message is LiveDmMessage =>
        !!message && (!options.peerJid || this.dmMessageMatchesPeer(message, options.peerJid, options.dmScope))
      );
    const outcomeMessages = outcomeAnchors.map((outcome) =>
      this.dmCallOutcomeAnchorToLiveMessage(outcome),
    );
    return {
      messages: [...messages, ...outcomeMessages],
      ...(page.first_id ? { firstArchiveId: page.first_id } : {}),
      ...(page.last_id ? { lastArchiveId: page.last_id } : {}),
      complete: page.is_complete,
    };
  }

  private dmCallOutcomeAnchorToLiveMessage(outcome: ArchivedDmCallOutcome): LiveDmMessage {
    const { anchor, terminalMessage } = outcome;
    const card = buildDmCallOutcomeAnchor(anchor, this.deps.sessionJid());
    const wireIds = rawMessageSeenIds(terminalMessage, this.dmStanzaIdAuthorities());
    return {
      id: card.id,
      ...(terminalMessage.mam_id ? { archiveId: terminalMessage.mam_id } : {}),
      peerJid: anchor.peerBareJid,
      fromJid: card.authorJid ?? anchor.peerBareJid,
      nick: card.author,
      body: card.body,
      createdAt: card.createdAt,
      createdAtSource: "archive",
      type: "message",
      ...(wireIds.length > 0 ? { wireIds } : {}),
      ...(card.threadId ? { threadId: card.threadId } : {}),
      ...(card.callThread ? { callThread: card.callThread } : {}),
    };
  }

  private applyDmCallEventsFromMamPage(
    page: WasmMamPage | null | undefined,
    selfBare = this.selfBare(),
    options: { since?: string; seenIds?: ReadonlyArray<string>; publishOutcome?: boolean; peerJid?: string; dmScope?: DmConversationScope } = {},
  ): ArchivedDmCallOutcome[] {
    // DM call state is bare-peer scoped. Until it is occupant scoped end to
    // end, replaying MUC-PM call events would alias every nick in the room.
    if (this.isMucPmPeer(options.peerJid ?? "", options.dmScope)) return [];
    const outcomeAnchors: ArchivedDmCallOutcome[] = [];
    for (const message of page?.messages ?? []) {
      if (!message.call_event) continue;
      const rawCounterpart = this.rawDmCounterpart(message);
      // The account archive tuple is authoritative before service discovery:
      // ordinary server DM rows use a bare remote endpoint; a full remote
      // endpoint is a MUC-PM occupant. Global hydration has no requested peer
      // or persisted scope, so fail closed on every resource-qualified row.
      if (!options.peerJid && !options.dmScope && resourceOf(rawCounterpart)) continue;
      // Global personal-history hydration has no requested peer context. The
      // raw remote endpoint still identifies MUC-PM rows, which DM call state
      // cannot safely represent until it is keyed by full occupant JID.
      if (this.isMucPmPeer(rawCounterpart)) continue;
      if (options.peerJid && !this.rawDmMessageMatchesPeer(message, options.peerJid, options.dmScope)) continue;
      if (shouldSkipRawCatchupMessage(message, this.dmStanzaIdAuthorities(), options.since, options.seenIds)) continue;
      const outcomeAnchor = applyDmCallEvent({
        event: message.call_event,
        selfBareJid: selfBare,
        selfFullJid: this.deps.fullJid(),
        to: message.to,
        timestamp: message.timestamp,
        publishOutcome: options.publishOutcome,
      });
      if (outcomeAnchor) outcomeAnchors.push({ anchor: outcomeAnchor, terminalMessage: message });
    }
    return outcomeAnchors;
  }

  /** Apply the #1256 occupant re-keying to a converted DM message. */
  private applyMucPmClassification(
    raw: WasmMessage,
    converted: LiveDmMessage,
    requestedPeerJid?: string,
    dmScope?: DmConversationScope,
  ): LiveDmMessage {
    const requestedOccupant = requestedPeerJid
      && this.isMucPmPeer(requestedPeerJid, dmScope)
      && fullJidIdentityKey(this.rawDmCounterpart(raw)) === fullJidIdentityKey(requestedPeerJid)
      ? { occupantJid: requestedPeerJid, nick: resourceOf(requestedPeerJid) }
      : undefined;
    const occupant = requestedOccupant ?? this.deps.classifyMucPm?.(raw);
    if (!occupant) return converted;
    const isSelf = barePeerJid(raw.from ?? "") === this.selfBare();
    return {
      ...converted,
      peerJid: occupant.occupantJid,
      mucPm: true,
      ...(isSelf ? {} : { nick: occupant.nick }),
    };
  }

  private dmMessageMatchesPeer(message: LiveDmMessage, peerJid: string, dmScope?: DmConversationScope): boolean {
    // #1281: a MUC-PM conversation is the full occupant JID. Bare matching
    // would alias every nick in the room; conversely, ordinary DMs retain
    // their established bare-JID conversation identity.
    if (this.isMucPmPeer(peerJid, dmScope)) {
      return message.mucPm === true
        && fullJidIdentityKey(message.peerJid) === fullJidIdentityKey(peerJid);
    }
    return message.mucPm !== true && bareJidKey(message.peerJid) === bareJidKey(peerJid);
  }

  private rawDmMessageMatchesPeer(message: WasmMessage, peerJid: string, dmScope?: DmConversationScope): boolean {
    if (this.isMucPmPeer(peerJid, dmScope)) {
      return fullJidIdentityKey(this.rawDmCounterpart(message)) === fullJidIdentityKey(peerJid);
    }
    const occupant = this.deps.classifyMucPm?.(message);
    if (occupant) return false;
    const rawPeer = this.rawDmCounterpart(message);
    // The persisted scope describes the requested conversation, not the raw
    // archive endpoint. Keep authoritative endpoint classification independent
    // so a legacy bare-room cursor cannot admit full occupant rows merely
    // because its old cursor defaulted to account scope.
    if (this.isMucPmPeer(rawPeer) && bareJidKey(rawPeer) === bareJidKey(peerJid)) return false;
    return bareJidKey(rawPeer) === bareJidKey(peerJid);
  }

  private rawDmCounterpart(message: WasmMessage): string {
    const from = message.from ?? "";
    return bareJidKey(from) === bareJidKey(this.selfBare()) ? (message.to ?? "") : from;
  }

  private dmArchivePeerJid(peerJid: string, dmScope?: DmConversationScope): string {
    return this.isMucPmPeer(peerJid, dmScope) ? peerJid : barePeerJid(peerJid);
  }

  private recordRoomWatermarks(messages: ReadonlyArray<LiveRoomMessage>) {
    for (const message of messages) {
      this.deps.catchup.recordRoomSeen(message.roomJid, message.createdAt, message.archiveId, messageSeenIds(message));
    }
  }

  private recordDmWatermarks(messages: ReadonlyArray<LiveDmMessage>) {
    for (const message of messages) {
      this.deps.catchup.recordDmSeen(message.peerJid, message.createdAt, message.archiveId, messageSeenIds(message), message.mucPm ? "muc-occupant" : "account");
    }
  }

  async queryMam(spaceId: string, channelId: string, max = 50): Promise<LiveRoomMessage[]> {
    const page = await this.queryMamPage(spaceId, channelId, max, { type: "latest" });
    return page.messages;
  }

  async queryMamPage(spaceId: string, channelId: string, max = 100, pageParam: MamPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> {
    const { xmpp, roomJid } = await this.deps.ensureRoomReady(spaceId, channelId);
    const page = await xmpp.fetch_room_history_page?.(roomJid, max, pageParam);
    if (!page) return { messages: [], complete: true };
    const result = this.roomPageToMessages(page);
    this.recordRoomWatermarks(result.messages);
    return result;
  }

  async queryMamByThread(spaceId: string, channelId: string, threadId: string, max = 100): Promise<LiveRoomMessage[]> {
    const { xmpp, roomJid } = await this.deps.ensureRoomReady(spaceId, channelId);
    const page = await xmpp.fetch_room_history_by_thread?.(roomJid, threadId, max, null);
    if (!page) return [];
    const result = this.roomPageToMessages(page);
    this.recordRoomWatermarks(result.messages);
    return result.messages;
  }

  async queryMamThreadPage(spaceId: string, channelId: string, threadId: string, max = 100, pageParam: MamThreadPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> {
    if (!threadId) return { messages: [], complete: true };
    const { xmpp, roomJid } = await this.deps.ensureRoomReady(spaceId, channelId);
    const page = await xmpp.fetch_room_history_by_thread?.(roomJid, threadId, max, pageParam.type === "before" ? pageParam.before : null);
    if (!page) return { messages: [], complete: true };
    const result = this.roomPageToMessages(page);
    this.recordRoomWatermarks(result.messages);
    return result;
  }

  async searchMessages(channelId: string, query: string, max = 20): Promise<MessageSearchResult[]> {
    if (!query.trim()) return [];
    const xmpp = await this.deps.requireConnectedXmpp();
    const page = await xmpp.search_room_history?.(this.deps.roomJidForChannel(channelId), query, max);
    const parsed = page ? this.roomPageToMessages(page).messages : [];
    return parsed.filter((message) => !!message.body).map((message, index) => ({ id: message.id, ...(page?.messages[index]?.mam_id ? { archiveId: page.messages[index].mam_id } : {}), nick: message.nick, body: message.body, createdAt: message.createdAt, ...(message.threadId ? { threadId: message.threadId } : {}), ...(message.parentThreadId ? { parentThreadId: message.parentThreadId } : {}), roomJid: message.roomJid }));
  }

  async queryPersonalMam(peerJid: string, max = 100, requestedScope?: DmConversationScope): Promise<LiveDmMessage[]> {
    const page = await this.queryPersonalMamPage(peerJid, max, { type: "latest" }, requestedScope);
    return page.messages;
  }

  async queryPersonalMamPage(
    peerJid: string,
    max = 100,
    pageParam: MamPageParam = { type: "latest" },
    requestedScope?: DmConversationScope,
  ): Promise<MamHistoryPage<LiveDmMessage>> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const dmScope = requestedScope ?? this.deps.catchup.getDmScope(peerJid);
    const archivePeerJid = this.dmArchivePeerJid(peerJid, dmScope);
    const page = await xmpp.fetch_dm_history_page?.(archivePeerJid, max, pageParam);
    if (!page) return { messages: [], complete: true };
    const result = this.dmPageToMessages(page, { peerJid: archivePeerJid, dmScope });
    this.recordDmWatermarks(result.messages);
    return result;
  }

  async queryPersonalMamThreadPage(
    peerJid: string,
    threadId: string,
    max = 100,
    pageParam: MamThreadPageParam = { type: "latest" },
    requestedScope?: DmConversationScope,
  ): Promise<MamHistoryPage<LiveDmMessage>> {
    if (!threadId) return { messages: [], complete: true };
    const xmpp = await this.deps.requireConnectedXmpp();
    const dmScope = requestedScope ?? this.deps.catchup.getDmScope(peerJid);
    const archivePeerJid = this.dmArchivePeerJid(peerJid, dmScope);
    const page = await xmpp.fetch_dm_history_by_thread?.(archivePeerJid, threadId, max, pageParam.type === "before" ? pageParam.before : null);
    if (!page) return { messages: [], complete: true };
    const result = this.dmPageToMessages(page, { applyCallEvents: false, peerJid: archivePeerJid, dmScope });
    this.recordDmWatermarks(result.messages);
    return result;
  }

  async hydrateRecentDmCallActivity(
    peerJid: string,
    options: DmCallActivityHydrationOptions = {},
  ): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.fetch_dm_history_page) return;
    if (
      this.deps.isMucPmPeer(peerJid)
      || this.deps.catchup.getDmScope(peerJid) === "muc-occupant"
    ) return;
    const peer = barePeerJid(peerJid);
    if (!peer) return;
    const sessionJid = this.deps.sessionJid();
    const fetchDmHistoryPage = xmpp.fetch_dm_history_page.bind(xmpp);
    const pages = await collectRecentMamPages(
      (max, pageParam) => fetchDmHistoryPage(peer, max, pageParam),
      options,
      () => this.deps.isCurrentConnected(xmpp, sessionJid),
    );
    if (!this.deps.isCurrentConnected(xmpp, sessionJid)) return;
    const selfBare = barePeerJid(sessionJid);
    for (const page of [...pages].reverse()) {
      this.applyDmCallEventsFromMamPage(page, selfBare, { publishOutcome: false });
    }
  }

  async hydrateRecentDmCallActivities(
    options: DmCallActivityHydrationOptions = {},
  ): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.fetch_personal_history_page) return;
    const sessionJid = this.deps.sessionJid();
    const fetchPersonalHistoryPage = xmpp.fetch_personal_history_page.bind(xmpp);
    const pages = await collectRecentMamPages(
      (max, pageParam) => fetchPersonalHistoryPage(max, pageParam),
      options,
      () => this.deps.isCurrentConnected(xmpp, sessionJid),
    );
    if (!this.deps.isCurrentConnected(xmpp, sessionJid)) return;
    const selfBare = barePeerJid(sessionJid);
    for (const page of [...pages].reverse()) {
      this.applyDmCallEventsFromMamPage(page, selfBare, { publishOutcome: false });
    }
  }

  async searchDmMessages(
    peerJid: string,
    query: string,
    max = 20,
    requestedScope?: DmConversationScope,
  ): Promise<MessageSearchResult[]> {
    if (!query.trim()) return [];
    const xmpp = await this.deps.requireConnectedXmpp();
    const dmScope = requestedScope ?? this.deps.catchup.getDmScope(peerJid);
    const archivePeerJid = this.dmArchivePeerJid(peerJid, dmScope);
    const page = await xmpp.search_dm_history?.(archivePeerJid, query, max);
    const selfBare = this.selfBare();
    const parsed = page?.messages
      .filter((archived) => this.rawDmMessageMatchesPeer(archived, archivePeerJid, dmScope))
      .map((archived) => {
        const decoded = dmMessageFromArchived(archived, selfBare, { trustedMediaOrigin: this.deps.trustedMediaOrigin() });
        return {
          archived,
          message: decoded ? this.applyMucPmClassification(archived, decoded, archivePeerJid, dmScope) : null,
        };
      })
      .filter((entry): entry is { archived: WasmArchivedMessage; message: LiveDmMessage } =>
        !!entry.message && this.dmMessageMatchesPeer(entry.message, archivePeerJid, dmScope)
      ) ?? [];
    return parsed.filter(({ message }) => !!message.body).map(({ archived, message }) => ({ id: message.id, ...(archived.mam_id ? { archiveId: archived.mam_id } : {}), nick: message.nick, body: message.body, createdAt: message.createdAt, ...(message.threadId ? { threadId: message.threadId } : {}), ...(message.parentThreadId ? { parentThreadId: message.parentThreadId } : {}), peerJid: message.peerJid }));
  }

  async runReconnectCatchup(
    xmpp: MamWasmClient,
    entries: ReadonlyArray<ReconnectCatchupEntry>,
    lifecycle: "fresh" | "resumed",
  ) {
    const sessionJid = this.deps.sessionJid();
    // Observe-only: measure how much work a single catch-up did and how
    // long it took (background-tab HUNG investigation). Keep stats local
    // because old/new xmpp handles can briefly overlap during reconnects.
    const startedAt = performance.now();
    const stats: CatchupRunStats = { pages: 0, pageFailures: 0, messages: 0 };
    let processedConversations = 0;
    let outcome: CatchupOutcome = "completed";
    try {
      for (const entry of entries) {
        if (!this.deps.isCurrentConnected(xmpp, sessionJid)) {
          outcome = "aborted";
          return;
        }
        try {
          if (entry.kind === "dm") {
            await this.runDmReconnectCatchup(xmpp, entry, sessionJid, stats);
          } else {
            await this.runRoomReconnectCatchup(xmpp, entry, sessionJid, stats);
          }
          if (!this.deps.isCurrentConnected(xmpp, sessionJid)) {
            outcome = "aborted";
            return;
          }
          processedConversations += 1;
        } catch (error) {
          if (!this.deps.isCurrentConnected(xmpp, sessionJid)) {
            outcome = "aborted";
            return;
          }
          outcome = "failed";
          processedConversations += 1;
          this.deps.emitError({
            kind: "history",
            recoverable: true,
            detail: `Reconnect catch-up failed for ${entry.key}`,
            cause: error,
          });
          // #1180: the FRESH lifecycle event promised this conversation
          // was covered, so its consumer skipped the wholesale reload.
          // Signal the failure so that reload can run as the fallback.
          // Serialization is per-conversation: the fallback fires after
          // THIS conversation's attempt failed (other entries may still
          // be paging their own conversations concurrently). On a
          // RESUMED lifecycle the stream itself is gap-free, so a
          // transient failure must not trigger a spurious reload — but
          // page-budget exhaustion (#1267 item 4) means the archive gap
          // beyond the cap was genuinely not replayed (long offline
          // window in a busy conversation), so the reload fallback is
          // the gap affordance there too.
          if (lifecycle === "fresh" || error instanceof CatchupPageBudgetExceededError) {
            // Plain `emit` with local isolation, not `emitSafe`: a throw
            // must neither propagate (it would reject the resume barrier
            // and abort the remaining entries) nor be mislabeled as a
            // telemetry-hook failure — this handler is the reload
            // fallback, so surface a failure on the typed error channel.
            try {
              this.deps.events.emit("catchupFailure", {
                kind: entry.kind,
                key: entry.key,
                ...(entry.kind === "dm" && entry.scope === "muc-occupant" ? { dmScope: entry.scope } : {}),
              });
            } catch (handlerError) {
              this.deps.emitError({
                kind: "history",
                recoverable: true,
                detail: `Reconnect catch-up fallback handler failed for ${entry.key}`,
                cause: handlerError,
              });
            }
          }
        }
      }
    } finally {
      this.deps.events.emitSafe("catchup", {
        conversations: entries.length,
        processedConversations,
        pages: stats.pages,
        pageFailures: stats.pageFailures,
        messages: stats.messages,
        durationMs: performance.now() - startedAt,
        outcome,
      });
    }
  }

  private async runDmReconnectCatchup(
    xmpp: MamWasmClient,
    entry: Extract<ReconnectCatchupEntry, { kind: "dm" }>,
    sessionJid: string,
    stats: CatchupRunStats,
  ) {
    if (!xmpp.fetch_dm_history_page) return;
    const archivePeerJid = this.dmArchivePeerJid(entry.key, entry.scope);
    if (entry.after) {
      let after: string | undefined = entry.after;
      const seenAfter = new Set<string>();
      let pageCount = 0;
      while (after) {
        if (seenAfter.has(after)) throw new Error(`Reconnect catch-up repeated archive cursor for ${entry.key}`);
        seenAfter.add(after);
        if (pageCount >= RECONNECT_CATCHUP_MAX_PAGES_PER_CONVERSATION) {
          throw new CatchupPageBudgetExceededError(`Reconnect catch-up exceeded ${RECONNECT_CATCHUP_MAX_PAGES_PER_CONVERSATION} pages for ${entry.key}`);
        }
        let page: WasmMamPage | null | undefined;
        try {
          page = await xmpp.fetch_dm_history_page(archivePeerJid, 100, { type: "after", after });
        } catch (error) {
          stats.pageFailures += 1;
          if (isMamCursorNotFound(classifyMamError(error))) {
            const since = entry.since ?? this.deps.catchup.getDmLastSeen(entry.key);
            if (since) await this.runDmTimestampCatchup(xmpp, entry.key, since, sessionJid, entry.seenIds, stats, entry.scope);
            return;
          }
          throw error;
        }
        pageCount += 1;
        if (!this.deps.isCurrentConnected(xmpp, sessionJid)) return;
        if (pageContainsArchiveId(page, after)) throw new Error(`Reconnect catch-up received non-advancing archive cursor for ${entry.key}`);
        const nextAfter = this.applyDmCatchupPage(page, entry.key, undefined, entry.seenIds, { stats, dmScope: entry.scope });
        if (isMamPageComplete(page)) return;
        if (!nextAfter || nextAfter === after) throw new Error(`Reconnect catch-up could not advance archive cursor for ${entry.key}`);
        after = nextAfter;
      }
      return;
    }
    const since = entry.since ?? this.deps.catchup.getDmLastSeen(entry.key);
    if (!since) return;
    await this.runDmTimestampCatchup(xmpp, entry.key, since, sessionJid, entry.seenIds, stats, entry.scope);
  }

  private async runRoomReconnectCatchup(
    xmpp: MamWasmClient,
    entry: { key: string; after?: string; since?: string; seenIds?: string[] },
    sessionJid: string,
    stats: CatchupRunStats,
  ) {
    if (!xmpp.fetch_room_history_page) return;
    if (entry.after) {
      let after: string | undefined = entry.after;
      const seenAfter = new Set<string>();
      let pageCount = 0;
      while (after) {
        if (seenAfter.has(after)) throw new Error(`Reconnect catch-up repeated archive cursor for ${entry.key}`);
        seenAfter.add(after);
        if (pageCount >= RECONNECT_CATCHUP_MAX_PAGES_PER_CONVERSATION) {
          throw new CatchupPageBudgetExceededError(`Reconnect catch-up exceeded ${RECONNECT_CATCHUP_MAX_PAGES_PER_CONVERSATION} pages for ${entry.key}`);
        }
        let page: WasmMamPage | null | undefined;
        try {
          page = await xmpp.fetch_room_history_page(entry.key, 100, { type: "after", after });
        } catch (error) {
          stats.pageFailures += 1;
          if (isMamCursorNotFound(classifyMamError(error))) {
            const since = entry.since ?? this.deps.catchup.getRoomLastSeen(entry.key);
            if (since) await this.runRoomTimestampCatchup(xmpp, entry.key, since, sessionJid, entry.seenIds, stats);
            return;
          }
          throw error;
        }
        pageCount += 1;
        if (!this.deps.isCurrentConnected(xmpp, sessionJid)) return;
        if (pageContainsArchiveId(page, after)) throw new Error(`Reconnect catch-up received non-advancing archive cursor for ${entry.key}`);
        const nextAfter = this.applyRoomCatchupPage(page, undefined, entry.seenIds, stats);
        if (isMamPageComplete(page)) return;
        if (!nextAfter || nextAfter === after) throw new Error(`Reconnect catch-up could not advance archive cursor for ${entry.key}`);
        after = nextAfter;
      }
      return;
    }
    const since = entry.since ?? this.deps.catchup.getRoomLastSeen(entry.key);
    if (!since) return;
    await this.runRoomTimestampCatchup(xmpp, entry.key, since, sessionJid, entry.seenIds, stats);
  }

  private async runDmTimestampCatchup(
    xmpp: MamWasmClient,
    peerJid: string,
    since: string,
    sessionJid: string,
    seenIds?: ReadonlyArray<string>,
    stats?: CatchupRunStats,
    dmScope?: DmConversationScope,
  ) {
    const archivePeerJid = this.dmArchivePeerJid(peerJid, dmScope);
    let pageParam: MamPageParam = { type: "latest" };
    const seenBefore = new Set<string>();
    const pages: WasmMamPage[] = [];
    // Apply the pages collected so far, oldest-first. Called on normal
    // completion and, on budget exhaustion, BEFORE the throw — so a
    // resumed session (whose fallback reload is fresh-only) still gets
    // the most-recent pages it fetched, matching the forward loop which
    // applies incrementally (#1221).
    const applyCollected = () => {
      const selfBare = this.selfBare();
      for (const page of [...pages].reverse()) {
        this.applyDmCatchupPage(page, peerJid, since, seenIds, { applyCallEvents: false, stats, dmScope });
      }
      for (const page of [...pages].reverse()) {
        this.applyDmCallEventsFromMamPage(page, selfBare, { since, seenIds, peerJid, dmScope });
      }
    };
    while (true) {
      let page: WasmMamPage | null | undefined;
      try {
        page = await xmpp.fetch_dm_history_page?.(archivePeerJid, 100, pageParam);
      } catch (error) {
        if (stats) stats.pageFailures += 1;
        throw error;
      }
      if (!this.deps.isCurrentConnected(xmpp, sessionJid)) return;
      if (page) pages.push(page);
      if (isMamPageComplete(page) || pageCrossesSince(page, since)) break;
      if (pages.length >= RECONNECT_CATCHUP_MAX_PAGES_PER_CONVERSATION) {
        applyCollected();
        throw new CatchupPageBudgetExceededError(`Reconnect catch-up exceeded ${RECONNECT_CATCHUP_MAX_PAGES_PER_CONVERSATION} pages for ${peerJid}`);
      }
      const firstArchiveId = pageFirstArchiveId(page);
      if (!firstArchiveId) throw new Error(`Reconnect catch-up could not page backward for ${peerJid}`);
      if (seenBefore.has(firstArchiveId)) throw new Error(`Reconnect catch-up repeated backward archive cursor for ${peerJid}`);
      seenBefore.add(firstArchiveId);
      pageParam = { type: "before", before: firstArchiveId };
    }
    applyCollected();
  }

  private async runRoomTimestampCatchup(
    xmpp: MamWasmClient,
    roomJid: string,
    since: string,
    sessionJid: string,
    seenIds?: ReadonlyArray<string>,
    stats?: CatchupRunStats,
  ) {
    let pageParam: MamPageParam = { type: "latest" };
    const seenBefore = new Set<string>();
    const pages: WasmMamPage[] = [];
    // Apply the pages collected so far, oldest-first — on normal
    // completion and, on budget exhaustion, BEFORE the throw so a resumed
    // session still gets the most-recent pages it fetched (#1221).
    const applyCollected = () => {
      for (const page of [...pages].reverse()) {
        this.applyRoomCatchupPage(page, since, seenIds, stats);
      }
    };
    while (true) {
      let page: WasmMamPage | null | undefined;
      try {
        page = await xmpp.fetch_room_history_page?.(roomJid, 100, pageParam);
      } catch (error) {
        if (stats) stats.pageFailures += 1;
        throw error;
      }
      if (!this.deps.isCurrentConnected(xmpp, sessionJid)) return;
      if (page) pages.push(page);
      if (isMamPageComplete(page) || pageCrossesSince(page, since)) break;
      if (pages.length >= RECONNECT_CATCHUP_MAX_PAGES_PER_CONVERSATION) {
        applyCollected();
        throw new CatchupPageBudgetExceededError(`Reconnect catch-up exceeded ${RECONNECT_CATCHUP_MAX_PAGES_PER_CONVERSATION} pages for ${roomJid}`);
      }
      const firstArchiveId = pageFirstArchiveId(page);
      if (!firstArchiveId) throw new Error(`Reconnect catch-up could not page backward for ${roomJid}`);
      if (seenBefore.has(firstArchiveId)) throw new Error(`Reconnect catch-up repeated backward archive cursor for ${roomJid}`);
      seenBefore.add(firstArchiveId);
      pageParam = { type: "before", before: firstArchiveId };
    }
    applyCollected();
  }

  private applyDmCatchupPage(
    page: WasmMamPage | null | undefined,
    peerJid: string,
    since?: string,
    seenIds?: ReadonlyArray<string>,
    options: { applyCallEvents?: boolean; stats?: CatchupRunStats; dmScope?: DmConversationScope } = {},
  ): string | undefined {
    // Keep the server page boundary even when every decoded row is rejected.
    // RSM cursors describe the raw result set; deriving this from accepted
    // rows would strand paging on an all-sibling legacy/foreign page.
    let lastArchiveId = pageLastArchiveId(page);
    const selfBare = this.selfBare();
    if (options.stats) options.stats.pages += 1;
    const acceptedRaw = (page?.messages ?? []).filter((message) =>
      this.rawDmMessageMatchesPeer(message, peerJid, options.dmScope)
    );
    const accepted = acceptedRaw.flatMap((message) => {
      const decoded = dmMessageFromArchived(message, selfBare, { trustedMediaOrigin: this.deps.trustedMediaOrigin() });
      if (!decoded) return [];
      const converted = this.applyMucPmClassification(message, decoded, peerJid, options.dmScope);
      return this.dmMessageMatchesPeer(converted, peerJid, options.dmScope) ? [{ converted }] : [];
    });
    if (options.applyCallEvents !== false) {
      this.applyDmCallEventsFromMamPage(
        page ? { ...page, messages: acceptedRaw } : page,
        selfBare,
        { since, seenIds, peerJid, dmScope: options.dmScope },
      );
    }
    for (const { converted } of accepted) {
      if (shouldSkipCatchupMessage(converted, since, seenIds)) continue;
      this.deps.catchup.recordDmSeen(converted.peerJid, converted.createdAt, converted.archiveId, messageSeenIds(converted), converted.mucPm ? "muc-occupant" : "account");
      this.deps.events.emit("directMessage", converted);
      if (options.stats) options.stats.messages += 1;
      lastArchiveId ??= converted.archiveId;
    }
    return lastArchiveId;
  }

  private applyRoomCatchupPage(page: WasmMamPage | null | undefined, since?: string, seenIds?: ReadonlyArray<string>, stats?: CatchupRunStats): string | undefined {
    let lastArchiveId = pageLastArchiveId(page);
    if (stats) stats.pages += 1;
    for (const message of page?.messages ?? []) {
      const converted = roomMessageFromArchived(message, { trustedMediaOrigin: this.deps.trustedMediaOrigin() });
      if (!converted || shouldSkipCatchupMessage(converted, since, seenIds)) continue;
      this.deps.catchup.recordRoomSeen(converted.roomJid, converted.createdAt, converted.archiveId, messageSeenIds(converted));
      if (converted.roomJid !== this.deps.currentRoom() && isRoomActivityMessage(converted)) {
        this.deps.events.emit("activity", roomActivityEventFromMessage(converted));
      } else {
        this.deps.events.emit("message", converted);
      }
      if (stats) stats.messages += 1;
      lastArchiveId = converted.archiveId ?? lastArchiveId;
    }
    return lastArchiveId;
  }
}
