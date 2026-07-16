/**
 * Connection-support modules extracted from `BrowserXmppClient`
 * (stage-2 decomposition of `client.ts`):
 *
 * - `ReconnectScheduler` — exponential-backoff reconnect timer plus the
 *   reconnect-duration stopwatch feeding the `statusHook` telemetry meta.
 * - `ResumeStateStore` — XEP-0198 resume-state bookkeeping over
 *   `ResumePersistence`: the POD snapshot that survives a page reload
 *   and the live WASM handle that survives an in-context reconnect.
 * - `OfflineSendQueue` — the persisted offline outbound queue and its
 *   drain logic (`outbound-queue-store` is the storage layer; this owns
 *   in-flight/ack tracking and replay ordering).
 *
 * Each class owns its state and is constructed with exactly the
 * collaborators it needs from the client.
 */
import {
  countQueuedMessages,
  enqueueQueuedMessage,
  listQueuedMessages,
  listQueuedRoomMessages,
  removeQueuedMessage,
  type PersistedQueuedDmMessage,
} from "../outbound-queue-store";
import { barePeerJid } from "./jid";
import type { ClientEvents, TypedEventBus } from "./client-events";
import type { ResumePersistence } from "./resume-persistence";
import type { SendDirectMessageOptions, SendGroupMessageOptions } from "./send-types";
import type { XmppStatusSnapshot } from "./types";
import type { WasmSendMessageOutcome } from "./wasm-types";

export type XmppResumeState = {
  previd: string;
  inboundH: number;
  outboundH: number;
  maxResumeSeconds?: number;
  hasUnackedOutbound?: boolean;
  unhandledOutboundEntries?: XmppResumeEntry[];
  resource?: string;
};

type XmppResumeEntry = {
  stanza: string;
  sentAt: string;
};

export type XmppResumeStateHandle = import("@waddle/xmpp-client-wasm").WaddleResumeState;

export interface OutboundSendResult {
  id: string | null;
  state: "queued" | "sending";
}

export function browserOffline(): boolean {
  return typeof navigator !== "undefined" && navigator.onLine === false;
}

function sentStanzaIdFromWasmOutcome(result: string | WasmSendMessageOutcome | null | undefined): string | null {
  if (typeof result === "string") return result;
  if (!result || result.kind !== "sent") return null;
  const stanzaId = result.stanza_id ?? result.stanzaId ?? null;
  if (!stanzaId) throw new Error("XMPP send did not return a stanza id");
  return stanzaId;
}

class WasmSendFailureError extends Error {
  constructor(readonly kind: Exclude<WasmSendMessageOutcome["kind"], "sent">) {
    super(`XMPP send failed: ${kind}`);
  }
}

export function isNonRetryableWasmSendFailure(error: unknown): error is WasmSendFailureError {
  return error instanceof WasmSendFailureError
    && (error.kind === "invalid-recipient" || error.kind === "invalid-options");
}

function sentStanzaIdOrThrowFromWasmOutcome(result: string | WasmSendMessageOutcome | null | undefined): string | null {
  if (typeof result === "string" || !result || result.kind === "sent") {
    return sentStanzaIdFromWasmOutcome(result);
  }
  throw new WasmSendFailureError(result.kind);
}

export function compatWasmSendResult(result: string | WasmSendMessageOutcome): string | null {
  return sentStanzaIdOrThrowFromWasmOutcome(result);
}

function messageStanzaIdFromSerializedXml(xml: string): string | null {
  if (typeof DOMParser !== "undefined") {
    try {
      const doc = new DOMParser().parseFromString(xml, "application/xml");
      const root = doc.documentElement;
      if (root.localName === "message") return root.getAttribute("id");
    } catch {}
  }

  const match = xml.match(/^<message\b[^>]*\sid=(["'])([^"']+)\1/i);
  return match?.[2] ?? null;
}

function validResumeMaxSeconds(value: number | undefined): number | undefined {
  return typeof value === "number" && Number.isInteger(value) && value > 0 && value <= 0xFFFF_FFFF
    ? value
    : undefined;
}

export function applyResumeStateToWasmConfig(config: unknown, resumeState: XmppResumeState): void {
  const wasmConfig = config as {
    with_resume_state_entries_with_max?: (
      previd: string,
      inboundH: number,
      outboundH: number,
      entries: XmppResumeEntry[],
      maxResumeSeconds: number,
    ) => void;
    with_resume_state_entries?: (
      previd: string,
      inboundH: number,
      outboundH: number,
      entries: XmppResumeEntry[],
    ) => void;
    with_resume_state_with_max?: (
      previd: string,
      inboundH: number,
      outboundH: number,
      maxResumeSeconds: number,
    ) => void;
    with_resume_state?: (previd: string, inboundH: number, outboundH: number) => void;
  };
  const maxResumeSeconds = validResumeMaxSeconds(resumeState.maxResumeSeconds);
  if (
    resumeState.unhandledOutboundEntries?.length
    && maxResumeSeconds !== undefined
    && typeof wasmConfig.with_resume_state_entries_with_max === "function"
  ) {
    wasmConfig.with_resume_state_entries_with_max(
      resumeState.previd,
      resumeState.inboundH,
      resumeState.outboundH,
      resumeState.unhandledOutboundEntries,
      maxResumeSeconds,
    );
    return;
  }
  if (
    resumeState.unhandledOutboundEntries?.length
    && typeof wasmConfig.with_resume_state_entries === "function"
  ) {
    wasmConfig.with_resume_state_entries(
      resumeState.previd,
      resumeState.inboundH,
      resumeState.outboundH,
      resumeState.unhandledOutboundEntries,
    );
    return;
  }
  if (resumeState.unhandledOutboundEntries?.length) {
    throw new Error("WASM client cannot restore timestamped XEP-0198 resume entries");
  }
  if (maxResumeSeconds !== undefined && typeof wasmConfig.with_resume_state_with_max === "function") {
    wasmConfig.with_resume_state_with_max(
      resumeState.previd,
      resumeState.inboundH,
      resumeState.outboundH,
      maxResumeSeconds,
    );
    return;
  }
  wasmConfig.with_resume_state?.(resumeState.previd, resumeState.inboundH, resumeState.outboundH);
}

type ReconnectSchedulerDeps = {
  isDestroying: () => boolean;
  connect: () => Promise<void>;
  onScheduled: (info: { attempt: number; delayMs: number }) => void;
  /** #1164: the attempt budget ran out — the caller should surface a terminal error state. */
  onExhausted: () => void;
};

/**
 * #1164: reconnect attempts before the loop gives up. With the
 * 2s·2^n backoff capped at 60s this spends ~6 minutes retrying —
 * long enough to ride out a deploy or a network blip, short enough
 * that a dead session doesn't spin a "reconnecting" banner forever.
 * `resetAttempts()` (wired to session-ready) restores the budget.
 */
const MAX_RECONNECT_ATTEMPTS = 10;

/** Exponential-backoff reconnect timer + reconnect-duration stopwatch. */
export class ReconnectScheduler {
  private attempt = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private startedAt: number | null = null;

  constructor(private readonly deps: ReconnectSchedulerDeps) {}

  schedule(): void {
    if (this.deps.isDestroying() || this.timer) return;
    if (this.attempt >= MAX_RECONNECT_ATTEMPTS) {
      this.deps.onExhausted();
      return;
    }
    const delay = Math.min(2000 * (2 ** this.attempt), 60000);
    this.attempt += 1;
    this.deps.onScheduled({ attempt: this.attempt, delayMs: delay });
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.deps.connect().catch(() => undefined);
    }, delay);
  }

  clearTimer(): void {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  /**
   * True while a backoff retry timer is armed — the loop owns the next
   * attempt. Background `connect()` calls must not preempt it: every
   * fast-failing immediate attempt re-enters `schedule()` and burns an
   * attempt from the budget, so ten user interactions during a short
   * outage would exhaust it into a false terminal error.
   */
  hasPendingRetry(): boolean {
    return this.timer !== null;
  }

  resetAttempts(): void {
    this.attempt = 0;
  }

  /**
   * True once the attempt cap is spent and no retry timer is pending.
   * While a timer is still armed the loop is alive, so callers must not
   * treat it as exhausted. Caveat: the timer is nulled BEFORE the final
   * attempt's `connect()` is invoked, so this also reads true while
   * that last attempt is still in flight — callers gating on it must
   * first join any pending connect promise (see `connect()` in
   * `client.ts`) so a user action during that window rides the attempt
   * instead of fast-rejecting.
   */
  isExhausted(): boolean {
    return this.attempt >= MAX_RECONNECT_ATTEMPTS && this.timer === null;
  }

  /**
   * Track the reconnect-duration stopwatch across status transitions.
   * Returns the `statusHook` telemetry meta: `reconnectDurationMs` is
   * present exactly when this status completes a reconnect.
   */
  noteStatus(snap: XmppStatusSnapshot): { reconnectDurationMs?: number } {
    if (snap.state === "reconnecting") {
      if (this.startedAt === null) this.startedAt = performance.now();
      return {};
    }
    if (this.startedAt === null) return {};
    if (snap.state === "online") {
      const durationMs = performance.now() - this.startedAt;
      this.startedAt = null;
      return { reconnectDurationMs: durationMs };
    }
    this.startedAt = null;
    return {};
  }
}

/** Structural subset of the WASM client the resume store reads on disconnect. */
type ResumeStateSource = {
  get_resume_state?: () => XmppResumeState | null;
  get_resume_state_handle?: () => XmppResumeStateHandle | undefined;
};

/**
 * XEP-0198 resume-state bookkeeping. Holds the POD snapshot (the only
 * piece that survives a full page reload) and the live WASM handle
 * (which takes precedence for an in-context reconnect), and mirrors
 * them into `ResumePersistence`.
 */
export class ResumeStateStore {
  private stateValue: XmppResumeState | null = null;
  private handleValue: XmppResumeStateHandle | null = null;

  constructor(private readonly persistence: ResumePersistence) {}

  get state(): XmppResumeState | null {
    return this.stateValue;
  }

  get handle(): XmppResumeStateHandle | null {
    return this.handleValue;
  }

  /** Hydrate the SM state persisted by a prior tab session (one-shot). */
  consumePersisted(): XmppResumeState | null {
    this.stateValue = this.persistence.consumeSm();
    return this.stateValue;
  }

  setHandle(handle: XmppResumeStateHandle | null | undefined): void {
    if (this.handleValue && this.handleValue !== handle) {
      try {
        this.handleValue.free();
      } catch {}
    }
    this.handleValue = handle ?? null;
  }

  /** Drop the in-memory POD state and its persisted copy; the handle is untouched. */
  discardState(): void {
    this.stateValue = null;
    this.persistence.clearSm();
  }

  /** Full teardown: state, handle, persisted SM slot, and retained-room list. */
  clearAll(): void {
    this.stateValue = null;
    this.setHandle(null);
    this.persistence.clearSm();
    this.persistence.clearJoinedRooms();
  }

  /**
   * Capture resume state from a disconnecting WASM handle. Keeps the
   * captured state in this JS context only — the shared per-account
   * persisted SM slot is a pagehide handoff for true tab replacement;
   * writing it during ordinary disconnects would let another live tab
   * claim this same resource while this client is still reconnecting
   * with its in-memory handle.
   */
  captureFromDisconnect(source: ResumeStateSource, resource: string): XmppResumeState | null {
    this.setHandle(source.get_resume_state_handle?.() ?? null);
    const resumeState = source.get_resume_state?.() ?? null;
    this.stateValue = resumeState ? { ...resumeState, resource } : null;
    return this.stateValue;
  }

  persistForPageHide(
    liveState: XmppResumeState | null,
    resource: string,
    persistJoinedRooms: () => void,
  ): void {
    const state = liveState ?? this.stateValue;
    this.persistence.preparePagehideHandoff();
    if (state) {
      if (state.hasUnackedOutbound && !state.unhandledOutboundEntries?.length) {
        this.stateValue = null;
        this.persistence.clearSm();
        persistJoinedRooms();
        return;
      }
      const snapshot = { ...state, resource };
      this.stateValue = snapshot;
      this.persistence.saveSm(snapshot);
    }
    persistJoinedRooms();
  }
}

type OfflineSendQueueDeps = {
  /** Per-account outbound-queue-store scope (the session's bare JID). */
  queueScope: () => string;
  events: TypedEventBus<ClientEvents>;
  canUseConnectedSession: () => boolean;
  roomIsReady: (roomJid: string) => boolean;
  /** Why a send is being queued right now ("offline", "reconnecting", …). */
  enqueueReason: () => string;
  emitStatus: (snapshot: XmppStatusSnapshot) => void;
  /** Known nick → bare-JID mentions for a room, merged into queued room sends. */
  roomMemberJids: (roomJid: string) => Record<string, string>;
  sendDirect: (
    peerJid: string,
    body: string,
    opts: SendDirectMessageOptions & { id: string },
  ) => Promise<string | null>;
  sendRoom: (
    roomJid: string,
    body: string,
    opts: SendGroupMessageOptions & { id: string },
  ) => Promise<string | null>;
};

/**
 * Persisted offline outbound queue + drain logic. Owns in-flight and
 * resume-replay id tracking, the pending-send latency map behind the
 * `messageAcked` / `messageDeliveryFailed` telemetry hooks, and the
 * per-conversation flush coalescing promises.
 */
export class OfflineSendQueue {
  private readonly inflightQueuedIds = new Set<string>();
  private readonly resumeReplayQueuedIds = new Set<string>();
  private readonly pendingSendAt = new Map<string, { at: number; kind: "room" | "dm" }>();
  private directFlushPromise: Promise<void> | null = null;
  private readonly roomFlushes = new Map<string, Promise<void>>();

  constructor(private readonly deps: OfflineSendQueueDeps) {}

  persistedCount(): number {
    return countQueuedMessages(this.deps.queueScope());
  }

  /**
   * Mark a stable stanza id in-flight before the send promise can yield.
   * WASM may deliver the matching SM acknowledgement synchronously while
   * resolving that promise, so both persistence ownership and latency state
   * must already exist when the callback runs.
   */
  beginAttempt(id: string, kind: "room" | "dm"): void {
    const wasInflight = this.inflightQueuedIds.has(id);
    this.inflightQueuedIds.add(id);
    this.pendingSendAt.set(id, { at: performance.now(), kind });
    if (!wasInflight) this.emitQueueDepth();
  }

  /** A rejected or null send did not transfer responsibility to XEP-0198. */
  rollbackAttempt(id: string): void {
    const wasInflight = this.inflightQueuedIds.delete(id);
    this.pendingSendAt.delete(id);
    if (wasInflight) {
      this.deps.events.emit("queuedMessageStatus", id, "queued");
      this.emitQueueDepth();
    }
  }

  /** Fresh session: pre-resume in-flight sends will never be acked. */
  clearInflight(): void {
    const hadInflight = this.inflightQueuedIds.size > 0;
    this.inflightQueuedIds.clear();
    this.pendingSendAt.clear();
    if (hadInflight) this.emitQueueDepth();
  }

  /**
   * Native XEP-0198 replay: stanzas the WASM client will re-send itself
   * on resume are tracked so their acks clear the persisted queue copy.
   */
  seedFromResumeState(state: XmppResumeState | null | undefined): void {
    for (const entry of state?.unhandledOutboundEntries ?? []) {
      const id = messageStanzaIdFromSerializedXml(entry.stanza);
      if (id) {
        this.inflightQueuedIds.add(id);
        this.resumeReplayQueuedIds.add(id);
      }
    }
  }

  handleAck(id: string): void {
    const wasQueued = this.inflightQueuedIds.delete(id);
    const wasResumeReplay = this.resumeReplayQueuedIds.delete(id);
    // The callback can arrive after a reload or synchronously inside the
    // awaited send. Persisted identity is authoritative; ephemeral Set
    // membership is never a prerequisite for deleting the durable copy.
    const wasPersisted = removeQueuedMessage(this.deps.queueScope(), id);
    this.deps.events.emit("messageAck", id);
    const pending = this.pendingSendAt.get(id);
    if (pending) {
      this.pendingSendAt.delete(id);
      this.deps.events.emitSafe("messageAcked", id, { kind: pending.kind, latencyMs: performance.now() - pending.at });
    }
    if (wasQueued || wasResumeReplay || wasPersisted) this.emitQueueDepth();
  }

  handleFailed(id: string): void {
    const wasQueued = this.inflightQueuedIds.delete(id);
    const wasResumeReplay = this.resumeReplayQueuedIds.delete(id);
    if (wasResumeReplay) removeQueuedMessage(this.deps.queueScope(), id);
    this.deps.events.emit("messageDeliveryFailure", id);
    const pending = this.pendingSendAt.get(id);
    if (pending) {
      this.pendingSendAt.delete(id);
      this.deps.events.emitSafe("messageDeliveryFailed", id, { kind: pending.kind });
    }
    if (wasQueued || wasResumeReplay) this.emitQueueDepth();
  }

  discardNonRetryable(id: string): void {
    this.inflightQueuedIds.delete(id);
    this.resumeReplayQueuedIds.delete(id);
    removeQueuedMessage(this.deps.queueScope(), id);
    this.deps.events.emit("messageDeliveryFailure", id);
    const pending = this.pendingSendAt.get(id);
    if (pending) {
      this.pendingSendAt.delete(id);
      this.deps.events.emitSafe("messageDeliveryFailed", id, { kind: pending.kind });
    }
    this.emitQueueDepth();
  }

  queueRoomMessage(roomJid: string, body: string, opts: SendGroupMessageOptions): OutboundSendResult {
    const queuedId = opts.id ?? crypto.randomUUID();
    enqueueQueuedMessage(this.deps.queueScope(), {
      kind: "room",
      id: queuedId,
      createdAt: new Date().toISOString(),
      roomJid,
      body,
      ...(opts.markup?.length ? { markup: opts.markup } : {}),
      ...(opts.references?.length ? { references: opts.references } : {}),
      ...(opts.mentionJidsByNick ? { mentionJidsByNick: opts.mentionJidsByNick } : {}),
      ...(opts.files?.length ? { files: opts.files } : {}),
      ...(opts.replyTo ? { replyTo: opts.replyTo } : {}),
      ...(opts.threadId ? { threadId: opts.threadId } : {}),
      ...(opts.parentThreadId ? { parentThreadId: opts.parentThreadId } : {}),
      ...(opts.threadCreate ? { threadCreate: opts.threadCreate } : {}),
      ...(opts.threadReply ? { threadReply: opts.threadReply } : {}),
    });
    this.deps.events.emit("queuedMessageStatus", queuedId, "queued");
    this.noteQueuedMessage();
    this.deps.events.emitSafe("sendEnqueued", { kind: "room", reason: this.deps.enqueueReason() });
    this.emitQueueDepth();
    return { id: queuedId, state: "queued" };
  }

  queueDirectMessage(peerJid: string, body: string, opts: SendDirectMessageOptions): OutboundSendResult {
    const queuedId = opts.id ?? crypto.randomUUID();
    enqueueQueuedMessage(this.deps.queueScope(), {
      kind: "dm",
      id: queuedId,
      createdAt: new Date().toISOString(),
      // #1256: a MUC-PM address is the FULL occupant JID and must be
      // preserved verbatim — bare-folding it would drain the reply to
      // the room bare JID (a broadcast).
      peerJid: opts.mucPm ? peerJid : barePeerJid(peerJid),
      ...(opts.mucPm ? { mucPm: true } : {}),
      body,
      ...(opts.markup?.length ? { markup: opts.markup } : {}),
      ...(opts.references?.length ? { references: opts.references } : {}),
      ...(opts.files?.length ? { files: opts.files } : {}),
      ...(opts.replyTo ? { replyTo: opts.replyTo } : {}),
      ...(opts.threadId ? { threadId: opts.threadId } : {}),
      ...(opts.parentThreadId ? { parentThreadId: opts.parentThreadId } : {}),
    });
    this.deps.events.emit("queuedMessageStatus", queuedId, "queued");
    this.noteQueuedMessage();
    this.deps.events.emitSafe("sendEnqueued", { kind: "dm", reason: this.deps.enqueueReason() });
    this.emitQueueDepth();
    return { id: queuedId, state: "queued" };
  }

  /** Persist an optimistic live room send so a crash before the ack replays it. */
  persistPendingRoomSend(roomJid: string, body: string, opts: SendGroupMessageOptions & { id: string }): void {
    enqueueQueuedMessage(this.deps.queueScope(), {
      kind: "room",
      id: opts.id,
      createdAt: new Date().toISOString(),
      roomJid,
      body,
      ...(opts.markup?.length ? { markup: opts.markup } : {}),
      ...(opts.references?.length ? { references: opts.references } : {}),
      ...(opts.mentionJidsByNick ? { mentionJidsByNick: opts.mentionJidsByNick } : {}),
      ...(opts.files?.length ? { files: opts.files } : {}),
      ...(opts.replyTo ? { replyTo: opts.replyTo } : {}),
      ...(opts.threadId ? { threadId: opts.threadId } : {}),
      ...(opts.parentThreadId ? { parentThreadId: opts.parentThreadId } : {}),
      ...(opts.threadCreate ? { threadCreate: opts.threadCreate } : {}),
      ...(opts.threadReply ? { threadReply: opts.threadReply } : {}),
    });
    this.emitQueueDepth();
  }

  /** Persist an optimistic live DM send so a crash before the ack replays it. */
  persistPendingDirectSend(peerJid: string, body: string, opts: SendDirectMessageOptions & { id: string }): void {
    enqueueQueuedMessage(this.deps.queueScope(), {
      kind: "dm",
      id: opts.id,
      createdAt: new Date().toISOString(),
      // #1256: see queueDirectMessage — occupant JIDs stay verbatim.
      peerJid: opts.mucPm ? peerJid : barePeerJid(peerJid),
      ...(opts.mucPm ? { mucPm: true } : {}),
      body,
      ...(opts.markup?.length ? { markup: opts.markup } : {}),
      ...(opts.references?.length ? { references: opts.references } : {}),
      ...(opts.files?.length ? { files: opts.files } : {}),
      ...(opts.replyTo ? { replyTo: opts.replyTo } : {}),
      ...(opts.threadId ? { threadId: opts.threadId } : {}),
      ...(opts.parentThreadId ? { parentThreadId: opts.parentThreadId } : {}),
    });
    this.emitQueueDepth();
  }

  async flushDirect(): Promise<void | undefined> {
    if (this.directFlushPromise) return this.directFlushPromise;
    if (!this.deps.canUseConnectedSession()) return;
    const promise = (async () => {
      const entries = listQueuedMessages(this.deps.queueScope()).filter((entry): entry is PersistedQueuedDmMessage => entry.kind === "dm");
      for (const entry of entries) {
        if (!this.deps.canUseConnectedSession()) break;
        if (this.inflightQueuedIds.has(entry.id)) continue;
        this.deps.events.emit("queuedMessageStatus", entry.id, "sending");
        this.beginAttempt(entry.id, "dm");
        let messageId: string | null;
        try {
          messageId = await this.deps.sendDirect(entry.mucPm ? entry.peerJid : barePeerJid(entry.peerJid), entry.body, { ...(entry.markup?.length ? { markup: entry.markup } : {}), ...(entry.references?.length ? { references: entry.references } : {}), ...(entry.files?.length ? { files: entry.files } : {}), ...(entry.replyTo ? { replyTo: entry.replyTo } : {}), ...(entry.threadId ? { threadId: entry.threadId } : {}), ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}), ...(entry.mucPm ? { mucPm: true } : {}), id: entry.id });
        } catch (error) {
          this.rollbackAttempt(entry.id);
          if (isNonRetryableWasmSendFailure(error)) {
            this.discardNonRetryable(entry.id);
            continue;
          }
          throw error;
        }
        if (!messageId) {
          this.rollbackAttempt(entry.id);
        } else if (messageId !== entry.id) {
          this.rollbackAttempt(entry.id);
          throw new Error("XMPP send returned a different stanza id");
        }
      }
    })();
    const trackedPromise = promise.finally(() => {
      if (this.directFlushPromise === trackedPromise) this.directFlushPromise = null;
    });
    this.directFlushPromise = trackedPromise;
    return trackedPromise;
  }

  async flushRoom(roomJid: string): Promise<void | undefined> {
    const inflight = this.roomFlushes.get(roomJid);
    if (inflight) return inflight;
    if (!this.deps.roomIsReady(roomJid)) return;
    const promise = (async () => {
      const entries = listQueuedRoomMessages(this.deps.queueScope(), roomJid);
      for (const entry of entries) {
        if (!this.deps.roomIsReady(roomJid)) break;
        if (this.inflightQueuedIds.has(entry.id)) continue;
        this.deps.events.emit("queuedMessageStatus", entry.id, "sending");
        this.beginAttempt(entry.id, "room");
        let messageId: string | null;
        try {
          messageId = await this.deps.sendRoom(roomJid, entry.body, { ...(entry.markup?.length ? { markup: entry.markup } : {}), ...(entry.references?.length ? { references: entry.references } : {}), mentionJidsByNick: { ...(entry.mentionJidsByNick ?? {}), ...this.deps.roomMemberJids(roomJid) }, ...(entry.files?.length ? { files: entry.files } : {}), ...(entry.replyTo ? { replyTo: entry.replyTo } : {}), ...(entry.threadId ? { threadId: entry.threadId } : {}), ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}), ...(entry.threadCreate ? { threadCreate: entry.threadCreate } : {}), ...(entry.threadReply ? { threadReply: entry.threadReply } : {}), id: entry.id });
        } catch (error) {
          this.rollbackAttempt(entry.id);
          if (isNonRetryableWasmSendFailure(error)) {
            this.discardNonRetryable(entry.id);
            continue;
          }
          throw error;
        }
        if (!messageId) {
          this.rollbackAttempt(entry.id);
        } else if (messageId !== entry.id) {
          this.rollbackAttempt(entry.id);
          throw new Error("XMPP send returned a different stanza id");
        }
      }
    })();
    const trackedPromise = promise.finally(() => {
      if (this.roomFlushes.get(roomJid) === trackedPromise) this.roomFlushes.delete(roomJid);
    });
    this.roomFlushes.set(roomJid, trackedPromise);
    return trackedPromise;
  }

  private emitQueueDepth(): void {
    const entries = listQueuedMessages(this.deps.queueScope());
    for (const kind of ["dm", "room"] as const) {
      const kindEntries = entries.filter((entry) => entry.kind === kind);
      const oldestCreatedAt = kindEntries.reduce<number | undefined>((oldest, entry) => {
        const createdAt = Date.parse(entry.createdAt);
        if (!Number.isFinite(createdAt)) return oldest;
        return oldest === undefined ? createdAt : Math.min(oldest, createdAt);
      }, undefined);
      this.deps.events.emitSafe("queueDepthChange", {
        kind,
        persisted: kindEntries.length,
        inflight: kindEntries.filter((entry) => this.inflightQueuedIds.has(entry.id)).length,
        ...(oldestCreatedAt === undefined
          ? {}
          : { oldestAgeMs: Math.max(0, Date.now() - oldestCreatedAt) }),
      });
    }
  }

  private noteQueuedMessage(): void {
    // #1164: a room-scoped queue while the session is healthy (we're
    // connected, the room just isn't joined yet) must not flip the
    // global connection banner to offline/reconnecting — the message
    // drains on join, not on reconnect. Only report a degraded status
    // when the session itself is unusable.
    if (this.deps.canUseConnectedSession()) return;
    const queueCount = countQueuedMessages(this.deps.queueScope());
    this.deps.emitStatus({
      state: browserOffline() ? "offline" : "reconnecting",
      detail: queueCount === 1 ? "Message queued until the connection returns" : `${queueCount} messages queued until the connection returns`,
    });
  }
}
