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
  enqueueQueuedMessage,
  QUEUE_TTL_MS,
  removeQueuedMessage,
  type PersistedQueuedDmMessage,
  type PersistedQueuedMessage,
  type PersistedQueuedRoomMessage,
} from "../outbound-queue-store";
import {
  IndexedDbDurableOutboundStore,
  MemoryDurableOutboundStore,
  OUTBOUND_CLAIM_LEASE_MS,
  committedOrThrow,
  createOutboundClaim,
  type DurableOutboundStore,
  type OutboundClaim,
} from "../outbound-durable-store";
import { barePeerJid } from "./jid";
import type { ClientEvents, TypedEventBus } from "./client-events";
import type {
  ResumePersistence,
  XmppResumeEntry,
  XmppResumeStanza,
} from "./resume-persistence";
import type { SendDirectMessageOptions, SendGroupMessageOptions } from "./send-types";
import type { XmppStatusSnapshot } from "./types";
import type { WasmSendMessageOutcome } from "./wasm-types";
import { reportError } from "../telemetry";

export type XmppResumeState = {
  previd: string;
  inboundH: number;
  outboundH: number;
  maxResumeSeconds?: number;
  hasUnackedOutbound?: boolean;
  unhandledOutboundEntries?: XmppResumeEntry[];
  resource?: string;
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

function messageStanzaIdFromResumeStanza(stanza: XmppResumeStanza): string | null {
  if (stanza.stanzaKind !== "message") return null;
  const root = stanza.tokens[0];
  if (
    root?.kind !== "start"
    || root.name.namespace !== "jabber:client"
    || root.name.localName !== "message"
  ) return null;
  return root.attributes.find((attribute) => (
    attribute.name.namespace === "" && attribute.name.localName === "id"
  ))?.value ?? null;
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
  async consumePersisted(): Promise<XmppResumeState | null> {
    this.stateValue = committedOrThrow(
      "sm-consume",
      await this.persistence.consumeSm(),
    );
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
  async discardState(): Promise<void> {
    this.stateValue = null;
    committedOrThrow("sm-clear", await this.persistence.clearSm());
  }

  /** Full teardown: state, handle, persisted SM slot, and retained-room list. */
  async clearAll(): Promise<void> {
    this.stateValue = null;
    this.setHandle(null);
    committedOrThrow("sm-clear-all", await this.persistence.clearSm());
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
        void this.persistence.clearSm().then((outcome) => {
          if (outcome.kind === "failed") {
            reportError("storage.write", outcome.cause, {
              recoverable: false,
              detail: "pagehide SM clear failed",
              storage_area: "sm-resume",
            });
          }
        });
        persistJoinedRooms();
        return;
      }
      const snapshot = { ...state, resource };
      this.stateValue = snapshot;
      void this.persistence.saveSm(snapshot).then((outcome) => {
        if (outcome.kind === "failed") {
          reportError("storage.write", outcome.cause, {
            recoverable: false,
            detail: "pagehide SM snapshot failed",
            storage_area: "sm-resume",
          });
        }
      });
    }
    persistJoinedRooms();
  }

  reclaimAfterPageShow(): void {
    this.persistence.reclaimPagehideOwnership();
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
  durableStore?: DurableOutboundStore;
};

type NativeQueueOwnership =
  | { kind: "resume-replay"; claim: OutboundClaim }
  | { kind: "fresh-fallback"; claim: OutboundClaim };

/**
 * Persisted offline outbound queue + drain logic. Owns in-flight and
 * resume-replay id tracking, the pending-send latency map behind the
 * `messageAcked` / `messageDeliveryFailed` telemetry hooks, and the
 * per-conversation flush coalescing promises.
 */
export class OfflineSendQueue {
  private readonly inflightQueuedIds = new Set<string>();
  /**
   * Stanzas whose next wire attempt is owned by the native XEP-0198 runtime.
   * A failed resume transfers them to the runtime's fresh-stream fallback;
   * the browser must retain the durable row without racing a second send.
   */
  private readonly nativeQueueOwnership = new Map<string, NativeQueueOwnership>();
  private readonly pendingSendAt = new Map<string, { at: number; kind: "room" | "dm" }>();
  private readonly durableMessages = new Map<string, PersistedQueuedMessage>();
  private readonly claims = new Map<string, OutboundClaim>();
  private readonly durableStore: DurableOutboundStore;
  private readonly durableHydration: Promise<void>;
  private readonly ownerId = crypto.randomUUID();
  private connectionGeneration = 0;
  private renewalTimer: ReturnType<typeof setInterval> | null = null;
  private directFlushPromise: Promise<void> | null = null;
  private readonly roomFlushes = new Map<string, Promise<void>>();

  constructor(private readonly deps: OfflineSendQueueDeps) {
    this.durableStore = deps.durableStore
      ?? (Reflect.has(globalThis, "Bun")
        ? new MemoryDurableOutboundStore()
        : new IndexedDbDurableOutboundStore());
    this.durableHydration = this.hydrateDurableMessages();
  }

  beginConnectionGeneration(generation?: number): number {
    this.connectionGeneration = generation ?? this.connectionGeneration + 1;
    return this.connectionGeneration;
  }

  private async hydrateDurableMessages(): Promise<void> {
    const messages = committedOrThrow(
      "hydrate-outbound-queue",
      await this.durableStore.list(this.deps.queueScope()),
    );
    const cutoff = Date.now() - QUEUE_TTL_MS;
    for (const message of messages) {
      const createdAt = Date.parse(message.createdAt);
      if (Number.isFinite(createdAt) && createdAt < cutoff) {
        committedOrThrow(
          "expire-outbound-row",
          await this.durableStore.delete(this.deps.queueScope(), message.id),
        );
        removeQueuedMessage(this.deps.queueScope(), message.id);
        continue;
      }
      this.durableMessages.set(message.id, message);
      // Presentation-only repair. A quota/private-mode failure here cannot
      // change durable resend, ordering, claim, or acknowledgement behavior.
      enqueueQueuedMessage(this.deps.queueScope(), message);
    }
  }

  private orderedDurableMessages(): PersistedQueuedMessage[] {
    return [...this.durableMessages.values()].sort((left, right) => {
      const createdAt = left.createdAt.localeCompare(right.createdAt);
      return createdAt !== 0 ? createdAt : left.id.localeCompare(right.id);
    });
  }

  persistedCount(): number {
    return this.durableMessages.size;
  }

  /**
   * Mark a stable stanza id in-flight before the send promise can yield.
   * WASM may deliver the matching SM acknowledgement synchronously while
   * resolving that promise, so both persistence ownership and latency state
   * must already exist when the callback runs.
   */
  beginAttempt(id: string, kind: "room" | "dm", claim?: OutboundClaim): void {
    const wasInflight = this.inflightQueuedIds.has(id);
    this.inflightQueuedIds.add(id);
    if (claim) this.trackClaim(id, claim);
    this.pendingSendAt.set(id, { at: performance.now(), kind });
    if (!wasInflight) this.emitQueueDepth();
  }

  /** A rejected or null send did not transfer responsibility to XEP-0198. */
  async rollbackAttempt(id: string): Promise<void> {
    await this.durableHydration;
    const claim = this.claims.get(id);
    if (claim) {
      const released = committedOrThrow(
        "release",
        await this.durableStore.release(this.deps.queueScope(), id, claim),
      );
      if (!released) throw new Error("Outbound queue claim changed before release");
      this.untrackClaim(id);
    }
    const wasInflight = this.inflightQueuedIds.delete(id);
    this.pendingSendAt.delete(id);
    if (wasInflight) {
      this.deps.events.emit("queuedMessageStatus", id, "queued");
      this.emitQueueDepth();
    }
  }

  /** Fresh session: pre-resume in-flight sends will never be acked. */
  async clearInflight(): Promise<void> {
    await this.durableHydration;
    for (const [id, ownership] of [...this.nativeQueueOwnership]) {
      if (ownership.kind !== "resume-replay") continue;
      const released = committedOrThrow(
        "release-resume-replay",
        await this.durableStore.release(this.deps.queueScope(), id, ownership.claim),
      );
      if (!released) throw new Error("Resume replay claim changed before fresh-session release");
      this.nativeQueueOwnership.delete(id);
      this.untrackClaim(id);
    }
    const hadInflight = this.inflightQueuedIds.size > 0;
    this.inflightQueuedIds.clear();
    this.pendingSendAt.clear();
    if (hadInflight) this.emitQueueDepth();
  }

  /**
   * Native XEP-0198 replay: stanzas the WASM client will re-send itself
   * on resume are tracked so their acks clear the persisted queue copy.
   */
  async seedFromResumeState(state: XmppResumeState | null | undefined): Promise<void> {
    await this.durableHydration;
    const authoritativeIds = new Set<string>();
    for (const entry of state?.unhandledOutboundEntries ?? []) {
      const id = messageStanzaIdFromResumeStanza(entry.stanza);
      if (id) {
        authoritativeIds.add(id);
        const existing = this.nativeQueueOwnership.get(id);
        if (existing?.kind === "fresh-fallback") continue;
        const claim = createOutboundClaim(
          this.ownerId,
          this.connectionGeneration,
          "resume-replay",
        );
        const adopted = committedOrThrow(
          "adopt-resume-replay",
          await this.durableStore.adopt(this.deps.queueScope(), id, claim),
        );
        if (!adopted) continue;
        this.inflightQueuedIds.add(id);
        this.nativeQueueOwnership.set(id, { kind: "resume-replay", claim: adopted });
        this.trackClaim(id, adopted);
      }
    }

    // The disconnect snapshot is authoritative for claims owned by this
    // browser instance. Anything absent can no longer be replayed by the
    // native runtime and must be durably returned to the browser queue.
    for (const [id, ownership] of [...this.nativeQueueOwnership]) {
      if (authoritativeIds.has(id) || ownership.kind === "fresh-fallback") continue;
      const released = committedOrThrow(
        "reconcile-resume-claim",
        await this.durableStore.release(this.deps.queueScope(), id, ownership.claim),
      );
      if (!released) throw new Error("Resume replay claim changed during reconciliation");
      this.nativeQueueOwnership.delete(id);
      this.inflightQueuedIds.delete(id);
      this.untrackClaim(id);
      this.deps.events.emit("queuedMessageStatus", id, "queued");
    }
    this.emitQueueDepth();
  }

  async handleAck(id: string): Promise<void> {
    await this.durableHydration;
    const wasPersisted = committedOrThrow(
      "ack-delete",
      await this.durableStore.delete(this.deps.queueScope(), id),
    );
    // An unrelated/duplicate acknowledgement is a true no-op: no projection
    // rewrite, no UI acknowledgement, and no telemetry claiming progress.
    if (!wasPersisted) return;
    this.durableMessages.delete(id);
    const wasQueued = this.inflightQueuedIds.delete(id);
    const wasNativeOwned = this.nativeQueueOwnership.delete(id);
    this.untrackClaim(id);
    removeQueuedMessage(this.deps.queueScope(), id);
    this.deps.events.emit("messageAck", id);
    const pending = this.pendingSendAt.get(id);
    if (pending) {
      this.pendingSendAt.delete(id);
      this.deps.events.emitSafe("messageAcked", id, { kind: pending.kind, latencyMs: performance.now() - pending.at });
    }
    if (wasQueued || wasNativeOwned || wasPersisted) this.emitQueueDepth();
  }

  async handleFailed(id: string): Promise<void> {
    await this.durableHydration;
    const wasQueued = this.inflightQueuedIds.delete(id);
    const nativeOwnership = this.nativeQueueOwnership.get(id);
    if (nativeOwnership?.kind === "resume-replay") {
      // `<failed/>` ends only the resume attempt. The native runtime now owns
      // the same stanza id on its conformant fresh-stream fallback. Keep the
      // browser row crash-durable and keep browser flushing fenced off until
      // the native path acks it or reports a subsequent terminal failure.
      const transitioned = committedOrThrow(
        "resume-to-fallback",
        await this.durableStore.transition(
          this.deps.queueScope(),
          id,
          nativeOwnership.claim,
          "fresh-fallback",
        ),
      );
      if (!transitioned) throw new Error("Resume replay claim changed before fallback transfer");
      this.nativeQueueOwnership.set(id, { kind: "fresh-fallback", claim: transitioned });
      this.trackClaim(id, transitioned);
      this.deps.events.emit("queuedMessageStatus", id, "sending");
      if (wasQueued) this.emitQueueDepth();
      return;
    }
    const claim = nativeOwnership?.claim ?? this.claims.get(id);
    if (claim) {
      const released = committedOrThrow(
        "delivery-failure-release",
        await this.durableStore.release(this.deps.queueScope(), id, claim),
      );
      if (!released) throw new Error("Outbound claim changed before delivery failure");
    }
    const wasNativeOwned = this.nativeQueueOwnership.delete(id);
    this.untrackClaim(id);
    this.deps.events.emit("messageDeliveryFailure", id);
    const pending = this.pendingSendAt.get(id);
    if (pending) {
      this.pendingSendAt.delete(id);
      this.deps.events.emitSafe("messageDeliveryFailed", id, { kind: pending.kind });
    }
    if (wasQueued || wasNativeOwned) this.emitQueueDepth();
  }

  async discardNonRetryable(id: string): Promise<void> {
    await this.durableHydration;
    const removed = committedOrThrow(
      "terminal-delete",
      await this.durableStore.delete(this.deps.queueScope(), id),
    );
    if (!removed) return;
    this.durableMessages.delete(id);
    this.inflightQueuedIds.delete(id);
    this.nativeQueueOwnership.delete(id);
    this.untrackClaim(id);
    removeQueuedMessage(this.deps.queueScope(), id);
    this.deps.events.emit("messageDeliveryFailure", id);
    const pending = this.pendingSendAt.get(id);
    if (pending) {
      this.pendingSendAt.delete(id);
      this.deps.events.emitSafe("messageDeliveryFailed", id, { kind: pending.kind });
    }
    this.emitQueueDepth();
  }

  async queueRoomMessage(roomJid: string, body: string, opts: SendGroupMessageOptions): Promise<OutboundSendResult> {
    await this.durableHydration;
    const queuedId = opts.id ?? crypto.randomUUID();
    const message: PersistedQueuedMessage = {
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
    };
    committedOrThrow(
      "enqueue-room",
      await this.durableStore.persistReady(this.deps.queueScope(), message),
    );
    this.durableMessages.set(message.id, message);
    enqueueQueuedMessage(this.deps.queueScope(), message);
    this.deps.events.emit("queuedMessageStatus", queuedId, "queued");
    this.noteQueuedMessage();
    this.deps.events.emitSafe("sendEnqueued", { kind: "room", reason: this.deps.enqueueReason() });
    this.emitQueueDepth();
    return { id: queuedId, state: "queued" };
  }

  async queueDirectMessage(peerJid: string, body: string, opts: SendDirectMessageOptions): Promise<OutboundSendResult> {
    await this.durableHydration;
    const queuedId = opts.id ?? crypto.randomUUID();
    const message: PersistedQueuedMessage = {
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
    };
    committedOrThrow(
      "enqueue-direct",
      await this.durableStore.persistReady(this.deps.queueScope(), message),
    );
    this.durableMessages.set(message.id, message);
    enqueueQueuedMessage(this.deps.queueScope(), message);
    this.deps.events.emit("queuedMessageStatus", queuedId, "queued");
    this.noteQueuedMessage();
    this.deps.events.emitSafe("sendEnqueued", { kind: "dm", reason: this.deps.enqueueReason() });
    this.emitQueueDepth();
    return { id: queuedId, state: "queued" };
  }

  /** Persist an optimistic live room send so a crash before the ack replays it. */
  async persistPendingRoomSend(roomJid: string, body: string, opts: SendGroupMessageOptions & { id: string }): Promise<OutboundClaim> {
    await this.durableHydration;
    const message: PersistedQueuedMessage = {
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
    };
    const claim = createOutboundClaim(this.ownerId, this.connectionGeneration, "sending");
    const committedClaim = committedOrThrow(
      "persist-live-room",
      await this.durableStore.persistClaimed(this.deps.queueScope(), message, claim),
    );
    this.durableMessages.set(message.id, message);
    enqueueQueuedMessage(this.deps.queueScope(), message);
    this.trackClaim(opts.id, committedClaim);
    this.emitQueueDepth();
    return committedClaim;
  }

  /** Persist an optimistic live DM send so a crash before the ack replays it. */
  async persistPendingDirectSend(peerJid: string, body: string, opts: SendDirectMessageOptions & { id: string }): Promise<OutboundClaim> {
    await this.durableHydration;
    const message: PersistedQueuedMessage = {
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
    };
    const claim = createOutboundClaim(this.ownerId, this.connectionGeneration, "sending");
    const committedClaim = committedOrThrow(
      "persist-live-direct",
      await this.durableStore.persistClaimed(this.deps.queueScope(), message, claim),
    );
    this.durableMessages.set(message.id, message);
    enqueueQueuedMessage(this.deps.queueScope(), message);
    this.trackClaim(opts.id, committedClaim);
    this.emitQueueDepth();
    return committedClaim;
  }

  async flushDirect(): Promise<void | undefined> {
    await this.durableHydration;
    if (this.directFlushPromise) return this.directFlushPromise;
    if (!this.deps.canUseConnectedSession()) return;
    const promise = (async () => {
      const entries = this.orderedDurableMessages()
        .filter((entry): entry is PersistedQueuedDmMessage => entry.kind === "dm");
      for (const entry of entries) {
        if (!this.deps.canUseConnectedSession()) break;
        if (this.inflightQueuedIds.has(entry.id) || this.nativeQueueOwnership.has(entry.id)) continue;
        const requestedClaim = createOutboundClaim(this.ownerId, this.connectionGeneration, "sending");
        const claim = committedOrThrow(
          "claim-direct",
          await this.durableStore.claim(this.deps.queueScope(), entry.id, requestedClaim),
        );
        // Stable queue order: another tab owns the head, so later rows must
        // wait instead of overtaking it.
        if (!claim) break;
        this.deps.events.emit("queuedMessageStatus", entry.id, "sending");
        this.beginAttempt(entry.id, "dm", claim);
        let messageId: string | null;
        try {
          messageId = await this.deps.sendDirect(entry.mucPm ? entry.peerJid : barePeerJid(entry.peerJid), entry.body, { ...(entry.markup?.length ? { markup: entry.markup } : {}), ...(entry.references?.length ? { references: entry.references } : {}), ...(entry.files?.length ? { files: entry.files } : {}), ...(entry.replyTo ? { replyTo: entry.replyTo } : {}), ...(entry.threadId ? { threadId: entry.threadId } : {}), ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}), ...(entry.mucPm ? { mucPm: true } : {}), id: entry.id });
        } catch (error) {
          await this.rollbackAttempt(entry.id);
          if (isNonRetryableWasmSendFailure(error)) {
            await this.discardNonRetryable(entry.id);
            continue;
          }
          throw error;
        }
        if (!messageId) {
          await this.rollbackAttempt(entry.id);
        } else if (messageId !== entry.id) {
          await this.rollbackAttempt(entry.id);
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
    await this.durableHydration;
    const inflight = this.roomFlushes.get(roomJid);
    if (inflight) return inflight;
    if (!this.deps.roomIsReady(roomJid)) return;
    const promise = (async () => {
      const entries = this.orderedDurableMessages().filter(
        (entry): entry is PersistedQueuedRoomMessage =>
          entry.kind === "room" && entry.roomJid === roomJid,
      );
      for (const entry of entries) {
        if (!this.deps.roomIsReady(roomJid)) break;
        if (this.inflightQueuedIds.has(entry.id) || this.nativeQueueOwnership.has(entry.id)) continue;
        const requestedClaim = createOutboundClaim(this.ownerId, this.connectionGeneration, "sending");
        const claim = committedOrThrow(
          "claim-room",
          await this.durableStore.claim(this.deps.queueScope(), entry.id, requestedClaim),
        );
        if (!claim) break;
        this.deps.events.emit("queuedMessageStatus", entry.id, "sending");
        this.beginAttempt(entry.id, "room", claim);
        let messageId: string | null;
        try {
          messageId = await this.deps.sendRoom(roomJid, entry.body, { ...(entry.markup?.length ? { markup: entry.markup } : {}), ...(entry.references?.length ? { references: entry.references } : {}), mentionJidsByNick: { ...(entry.mentionJidsByNick ?? {}), ...this.deps.roomMemberJids(roomJid) }, ...(entry.files?.length ? { files: entry.files } : {}), ...(entry.replyTo ? { replyTo: entry.replyTo } : {}), ...(entry.threadId ? { threadId: entry.threadId } : {}), ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}), ...(entry.threadCreate ? { threadCreate: entry.threadCreate } : {}), ...(entry.threadReply ? { threadReply: entry.threadReply } : {}), id: entry.id });
        } catch (error) {
          await this.rollbackAttempt(entry.id);
          if (isNonRetryableWasmSendFailure(error)) {
            await this.discardNonRetryable(entry.id);
            continue;
          }
          throw error;
        }
        if (!messageId) {
          await this.rollbackAttempt(entry.id);
        } else if (messageId !== entry.id) {
          await this.rollbackAttempt(entry.id);
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

  private trackClaim(id: string, claim: OutboundClaim): void {
    this.claims.set(id, claim);
    if (this.renewalTimer) return;
    this.renewalTimer = setInterval(() => {
      void this.renewClaims().catch((error) => reportError("storage.write", error, {
        recoverable: false,
        detail: "outbound claim renewal failed",
        storage_area: "outbound-queue",
      }));
    }, Math.floor(OUTBOUND_CLAIM_LEASE_MS / 3));
    (this.renewalTimer as { unref?: () => void }).unref?.();
  }

  private untrackClaim(id: string): void {
    this.claims.delete(id);
    if (this.claims.size !== 0 || !this.renewalTimer) return;
    clearInterval(this.renewalTimer);
    this.renewalTimer = null;
  }

  private async renewClaims(): Promise<void> {
    for (const [id, claim] of [...this.claims]) {
      const renewed = committedOrThrow(
        "renew-claim",
        await this.durableStore.renew(
          this.deps.queueScope(),
          id,
          claim,
          Date.now() + OUTBOUND_CLAIM_LEASE_MS,
        ),
      );
      if (!renewed) {
        this.untrackClaim(id);
        this.inflightQueuedIds.delete(id);
        this.nativeQueueOwnership.delete(id);
        continue;
      }
      this.claims.set(id, renewed);
      const ownership = this.nativeQueueOwnership.get(id);
      if (ownership) {
        this.nativeQueueOwnership.set(id, { ...ownership, claim: renewed });
      }
    }
  }

  private emitQueueDepth(): void {
    const entries = this.orderedDurableMessages();
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
        inflight: kindEntries.filter(
          (entry) =>
            this.inflightQueuedIds.has(entry.id) || this.nativeQueueOwnership.has(entry.id),
        ).length,
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
    const queueCount = this.durableMessages.size;
    this.deps.emitStatus({
      state: browserOffline() ? "offline" : "reconnecting",
      detail: queueCount === 1 ? "Message queued until the connection returns" : `${queueCount} messages queued until the connection returns`,
    });
  }
}
