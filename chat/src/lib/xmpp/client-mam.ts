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
import { barePeerJid } from "./jid";
import type { ClientEvents, TypedEventBus } from "./client-events";
import { classifyMamError, isMamCursorNotFound } from "./mam";
import type { ReconnectCatchup } from "./reconnect-catchup";
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
import { dmMessageFromArchived, roomMessageFromArchived } from "./wasm-message-codecs";
import type { WasmArchivedMessage, WasmMamPage, WasmMessage } from "./wasm-types";

const DM_CALL_ACTIVITY_PAGE_SIZE = 100;
const DM_CALL_ACTIVITY_MAX_PAGES = 50;

export type DmCallActivityHydrationOptions = { since?: string; pageSize?: number; maxPages?: number };

type CatchupRunStats = { pages: number; messages: number };
type CatchupOutcome = "completed" | "aborted" | "failed";

type ReconnectCatchupEntry = { kind: "dm" | "room"; key: string; after?: string; since?: string; seenIds?: string[] };

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

export function rawMessageSeenIds(message: WasmMessage): string[] {
  return Array.from(new Set([
    message.id,
    message.origin_id,
    message.stanza_id,
    ...(message.stanza_ids?.map((stanzaId) => stanzaId.id) ?? []),
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
  since?: string,
  seenIds?: ReadonlyArray<string>,
): boolean {
  const seen = new Set(seenIds ?? []);
  if (seen.size > 0 && rawMessageSeenIds(message).some((id) => seen.has(id))) return true;
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
  /** Connect + switch/join so room history queries target a ready room. */
  ensureRoomReady: (spaceId: string, channelId: string) => Promise<void>;
  roomJidForChannel: (channelId: string) => string;
  /** Identity + liveness gate for a specific WASM handle mid-pagination. */
  isCurrentConnected: (xmpp: MamWasmClient, sessionJid: string) => boolean;
};

export class MamPager {
  constructor(private readonly deps: MamPagerDeps) {}

  private selfBare(): string {
    return barePeerJid(this.deps.sessionJid());
  }

  private roomPageToMessages(page: WasmMamPage): MamHistoryPage<LiveRoomMessage> {
    return { messages: page.messages.map((message) => roomMessageFromArchived(message, { trustedMediaOrigin: this.deps.trustedMediaOrigin() })).filter((message): message is LiveRoomMessage => !!message), ...(page.first_id ? { firstArchiveId: page.first_id } : {}), ...(page.last_id ? { lastArchiveId: page.last_id } : {}), complete: page.is_complete };
  }

  private dmPageToMessages(
    page: WasmMamPage,
    options: { applyCallEvents?: boolean } = {},
  ): MamHistoryPage<LiveDmMessage> {
    const selfBare = this.selfBare();
    const outcomeAnchors = options.applyCallEvents === false
      ? []
      : this.applyDmCallEventsFromMamPage(page, selfBare, { publishOutcome: false });
    const messages = page.messages
      .map((message) => dmMessageFromArchived(message, selfBare, { trustedMediaOrigin: this.deps.trustedMediaOrigin() }))
      .filter((message): message is LiveDmMessage => !!message);
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
    const wireIds = rawMessageSeenIds(terminalMessage);
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
    options: { since?: string; seenIds?: ReadonlyArray<string>; publishOutcome?: boolean } = {},
  ): ArchivedDmCallOutcome[] {
    const outcomeAnchors: ArchivedDmCallOutcome[] = [];
    for (const message of page?.messages ?? []) {
      if (!message.call_event) continue;
      if (shouldSkipRawCatchupMessage(message, options.since, options.seenIds)) continue;
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

  private recordRoomWatermarks(messages: ReadonlyArray<LiveRoomMessage>) {
    for (const message of messages) {
      this.deps.catchup.recordRoomSeen(message.roomJid, message.createdAt, message.archiveId, messageSeenIds(message));
    }
  }

  private recordDmWatermarks(messages: ReadonlyArray<LiveDmMessage>) {
    for (const message of messages) {
      this.deps.catchup.recordDmSeen(message.peerJid, message.createdAt, message.archiveId, messageSeenIds(message));
    }
  }

  async queryMam(spaceId: string, channelId: string, max = 50): Promise<LiveRoomMessage[]> {
    const page = await this.queryMamPage(spaceId, channelId, max, { type: "latest" });
    return page.messages;
  }

  async queryMamPage(spaceId: string, channelId: string, max = 100, pageParam: MamPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> {
    await this.deps.ensureRoomReady(spaceId, channelId);
    const xmpp = await this.deps.requireConnectedXmpp();
    const page = await xmpp.fetch_room_history_page?.(this.deps.roomJidForChannel(channelId), max, pageParam);
    if (!page) return { messages: [], complete: true };
    const result = this.roomPageToMessages(page);
    this.recordRoomWatermarks(result.messages);
    return result;
  }

  async queryMamByThread(spaceId: string, channelId: string, threadId: string, max = 100): Promise<LiveRoomMessage[]> {
    await this.deps.ensureRoomReady(spaceId, channelId);
    const xmpp = await this.deps.requireConnectedXmpp();
    const page = await xmpp.fetch_room_history_by_thread?.(this.deps.roomJidForChannel(channelId), threadId, max, null);
    if (!page) return [];
    const result = this.roomPageToMessages(page);
    this.recordRoomWatermarks(result.messages);
    return result.messages;
  }

  async queryMamThreadPage(spaceId: string, channelId: string, threadId: string, max = 100, pageParam: MamThreadPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveRoomMessage>> {
    if (!threadId) return { messages: [], complete: true };
    await this.deps.ensureRoomReady(spaceId, channelId);
    const xmpp = await this.deps.requireConnectedXmpp();
    const page = await xmpp.fetch_room_history_by_thread?.(this.deps.roomJidForChannel(channelId), threadId, max, pageParam.type === "before" ? pageParam.before : null);
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

  async queryPersonalMam(peerJid: string, max = 100): Promise<LiveDmMessage[]> {
    const page = await this.queryPersonalMamPage(peerJid, max, { type: "latest" });
    return page.messages;
  }

  async queryPersonalMamPage(peerJid: string, max = 100, pageParam: MamPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveDmMessage>> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const page = await xmpp.fetch_dm_history_page?.(barePeerJid(peerJid), max, pageParam);
    if (!page) return { messages: [], complete: true };
    const result = this.dmPageToMessages(page);
    this.recordDmWatermarks(result.messages);
    return result;
  }

  async queryPersonalMamThreadPage(peerJid: string, threadId: string, max = 100, pageParam: MamThreadPageParam = { type: "latest" }): Promise<MamHistoryPage<LiveDmMessage>> {
    if (!threadId) return { messages: [], complete: true };
    const xmpp = await this.deps.requireConnectedXmpp();
    const page = await xmpp.fetch_dm_history_by_thread?.(barePeerJid(peerJid), threadId, max, pageParam.type === "before" ? pageParam.before : null);
    if (!page) return { messages: [], complete: true };
    const result = this.dmPageToMessages(page, { applyCallEvents: false });
    this.recordDmWatermarks(result.messages);
    return result;
  }

  async hydrateRecentDmCallActivity(
    peerJid: string,
    options: DmCallActivityHydrationOptions = {},
  ): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.fetch_dm_history_page) return;
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

  async searchDmMessages(peerJid: string, query: string, max = 20): Promise<MessageSearchResult[]> {
    if (!query.trim()) return [];
    const xmpp = await this.deps.requireConnectedXmpp();
    const page = await xmpp.search_dm_history?.(barePeerJid(peerJid), query, max);
    const selfBare = this.selfBare();
    const parsed = page?.messages
      .map((archived) => ({
        archived,
        message: dmMessageFromArchived(archived, selfBare, { trustedMediaOrigin: this.deps.trustedMediaOrigin() }),
      }))
      .filter((entry): entry is { archived: WasmArchivedMessage; message: LiveDmMessage } => !!entry.message) ?? [];
    return parsed.filter(({ message }) => !!message.body).map(({ archived, message }) => ({ id: message.id, ...(archived.mam_id ? { archiveId: archived.mam_id } : {}), nick: message.nick, body: message.body, createdAt: message.createdAt, ...(message.threadId ? { threadId: message.threadId } : {}), ...(message.parentThreadId ? { parentThreadId: message.parentThreadId } : {}), peerJid: message.peerJid }));
  }

  async runReconnectCatchup(
    xmpp: MamWasmClient,
    entries: ReadonlyArray<ReconnectCatchupEntry>,
  ) {
    const sessionJid = this.deps.sessionJid();
    // Observe-only: measure how much work a single catch-up did and how
    // long it took (background-tab HUNG investigation). Keep stats local
    // because old/new xmpp handles can briefly overlap during reconnects.
    const startedAt = performance.now();
    const stats: CatchupRunStats = { pages: 0, messages: 0 };
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
          // #1180: the fresh lifecycle event promised this conversation
          // was covered, so its consumer skipped the wholesale reload.
          // Signal the failure so that reload can run as the fallback —
          // after the failed attempt, never concurrently with it.
          this.deps.events.emitSafe("catchupFailure", { kind: entry.kind, key: entry.key });
        }
      }
    } finally {
      this.deps.events.emitSafe("catchup", {
        conversations: entries.length,
        processedConversations,
        pages: stats.pages,
        messages: stats.messages,
        durationMs: performance.now() - startedAt,
        outcome,
      });
    }
  }

  private async runDmReconnectCatchup(
    xmpp: MamWasmClient,
    entry: { key: string; after?: string; since?: string; seenIds?: string[] },
    sessionJid: string,
    stats: CatchupRunStats,
  ) {
    if (!xmpp.fetch_dm_history_page) return;
    if (entry.after) {
      let after: string | undefined = entry.after;
      const seenAfter = new Set<string>();
      while (after) {
        if (seenAfter.has(after)) throw new Error(`Reconnect catch-up repeated archive cursor for ${entry.key}`);
        seenAfter.add(after);
        let page: WasmMamPage | null | undefined;
        try {
          page = await xmpp.fetch_dm_history_page(entry.key, 100, { type: "after", after });
        } catch (error) {
          if (isMamCursorNotFound(classifyMamError(error))) {
            const since = entry.since ?? this.deps.catchup.getDmLastSeen(entry.key);
            if (since) await this.runDmTimestampCatchup(xmpp, entry.key, since, sessionJid, entry.seenIds, stats);
            return;
          }
          throw error;
        }
        if (!this.deps.isCurrentConnected(xmpp, sessionJid)) return;
        if (pageContainsArchiveId(page, after)) throw new Error(`Reconnect catch-up received non-advancing archive cursor for ${entry.key}`);
        const nextAfter = this.applyDmCatchupPage(page, undefined, entry.seenIds, { stats });
        if (isMamPageComplete(page)) return;
        if (!nextAfter || nextAfter === after) throw new Error(`Reconnect catch-up could not advance archive cursor for ${entry.key}`);
        after = nextAfter;
      }
      return;
    }
    const since = entry.since ?? this.deps.catchup.getDmLastSeen(entry.key);
    if (!since) return;
    await this.runDmTimestampCatchup(xmpp, entry.key, since, sessionJid, entry.seenIds, stats);
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
      while (after) {
        if (seenAfter.has(after)) throw new Error(`Reconnect catch-up repeated archive cursor for ${entry.key}`);
        seenAfter.add(after);
        let page: WasmMamPage | null | undefined;
        try {
          page = await xmpp.fetch_room_history_page(entry.key, 100, { type: "after", after });
        } catch (error) {
          if (isMamCursorNotFound(classifyMamError(error))) {
            const since = entry.since ?? this.deps.catchup.getRoomLastSeen(entry.key);
            if (since) await this.runRoomTimestampCatchup(xmpp, entry.key, since, sessionJid, entry.seenIds, stats);
            return;
          }
          throw error;
        }
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
  ) {
    let pageParam: MamPageParam = { type: "latest" };
    const seenBefore = new Set<string>();
    const pages: WasmMamPage[] = [];
    while (true) {
      const page = await xmpp.fetch_dm_history_page?.(peerJid, 100, pageParam);
      if (!this.deps.isCurrentConnected(xmpp, sessionJid)) return;
      if (page) pages.push(page);
      if (isMamPageComplete(page) || pageCrossesSince(page, since)) break;
      const firstArchiveId = pageFirstArchiveId(page);
      if (!firstArchiveId) throw new Error(`Reconnect catch-up could not page backward for ${peerJid}`);
      if (seenBefore.has(firstArchiveId)) throw new Error(`Reconnect catch-up repeated backward archive cursor for ${peerJid}`);
      seenBefore.add(firstArchiveId);
      pageParam = { type: "before", before: firstArchiveId };
    }
    const selfBare = this.selfBare();
    for (const page of [...pages].reverse()) {
      this.applyDmCatchupPage(page, since, seenIds, { applyCallEvents: false, stats });
    }
    for (const page of [...pages].reverse()) {
      this.applyDmCallEventsFromMamPage(page, selfBare, { since, seenIds });
    }
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
    while (true) {
      const page = await xmpp.fetch_room_history_page?.(roomJid, 100, pageParam);
      if (!this.deps.isCurrentConnected(xmpp, sessionJid)) return;
      if (page) pages.push(page);
      if (isMamPageComplete(page) || pageCrossesSince(page, since)) break;
      const firstArchiveId = pageFirstArchiveId(page);
      if (!firstArchiveId) throw new Error(`Reconnect catch-up could not page backward for ${roomJid}`);
      if (seenBefore.has(firstArchiveId)) throw new Error(`Reconnect catch-up repeated backward archive cursor for ${roomJid}`);
      seenBefore.add(firstArchiveId);
      pageParam = { type: "before", before: firstArchiveId };
    }
    for (const page of [...pages].reverse()) {
      this.applyRoomCatchupPage(page, since, seenIds, stats);
    }
  }

  private applyDmCatchupPage(
    page: WasmMamPage | null | undefined,
    since?: string,
    seenIds?: ReadonlyArray<string>,
    options: { applyCallEvents?: boolean; stats?: CatchupRunStats } = {},
  ): string | undefined {
    let lastArchiveId = pageLastArchiveId(page);
    const selfBare = this.selfBare();
    if (options.stats) options.stats.pages += 1;
    if (options.applyCallEvents !== false) {
      this.applyDmCallEventsFromMamPage(page, selfBare, { since, seenIds });
    }
    for (const message of page?.messages ?? []) {
      const converted = dmMessageFromArchived(message, selfBare, { trustedMediaOrigin: this.deps.trustedMediaOrigin() });
      if (!converted || shouldSkipCatchupMessage(converted, since, seenIds)) continue;
      this.deps.catchup.recordDmSeen(converted.peerJid, converted.createdAt, converted.archiveId, messageSeenIds(converted));
      this.deps.events.emit("directMessage", converted);
      if (options.stats) options.stats.messages += 1;
      lastArchiveId = converted.archiveId ?? lastArchiveId;
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
