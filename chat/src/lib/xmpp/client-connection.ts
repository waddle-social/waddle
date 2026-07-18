/**
 * Connection-support modules extracted from `BrowserXmppClient`
 * (stage-2 decomposition of `client.ts`):
 *
 * - `ReconnectScheduler` — exponential-backoff reconnect timer plus the
 *   reconnect-duration stopwatch feeding the `statusHook` telemetry meta.
 * - `ResumeStateStore` — XEP-0198 resume-state bookkeeping over
 *   `ResumePersistence`: one typed POD snapshot for both in-context reconnect
 *   and page reload.
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
  type PersistedQueuedMessage,
} from "../outbound-queue-store";
import {
  OUTBOUND_CLAIM_LEASE_MS,
  committedOrThrow,
  createOutboundClaim,
  type DurableOutcome,
  type DurableOutboundStore,
  type OutboundClaim,
  type OutboundOwnerActivation,
  type OutboundOwnerContext,
  type OutboundOwnerHint,
  type OutboundRowIdentity,
  type OutboundTerminalApplyResult,
  type OutboundTerminalIntent,
  outboundLane,
  roomOutboundLane,
} from "../xmpp-runtime/durable-contract";
import { IndexedDbDurableOutboundStore } from "../xmpp-runtime/indexeddb-durable-store";
import { barePeerJid } from "./jid";
import type { ClientEvents, TypedEventBus } from "./client-events";
import type {
  ResumePersistence,
  PersistedSmResumeState,
  XmppLifecycleId,
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
  unhandledOutboundEntries: XmppResumeEntry[];
  resource?: string;
};

export interface OutboundSendResult {
  id: string | null;
  state: "queued" | "sending";
}

export type PendingSendReservation =
  | { kind: "claimed"; claim: OutboundClaim }
  | { kind: "queued" };

export function browserOffline(): boolean {
  return typeof navigator !== "undefined" && navigator.onLine === false;
}

function sentStanzaIdFromWasmOutcome(result: WasmSendMessageOutcome | null | undefined): string | null {
  if (!result || result.kind !== "sent") return null;
  if (!result.stanza_id) throw new Error("XMPP send did not return a stanza id");
  return result.stanza_id;
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

function sentStanzaIdOrThrowFromWasmOutcome(result: WasmSendMessageOutcome | null | undefined): string | null {
  if (!result || result.kind === "sent") {
    return sentStanzaIdFromWasmOutcome(result);
  }
  throw new WasmSendFailureError(result.kind);
}

export function wasmSendMessageId(result: WasmSendMessageOutcome): string | null {
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

function resumeMaxSeconds(value: number | undefined): number | undefined {
  if (value === undefined) return undefined;
  if (!Number.isInteger(value) || value <= 0 || value > 0xFFFF_FFFF) {
    throw new TypeError("XEP-0198 maxResumeSeconds must be a positive u32");
  }
  return value;
}

function canonicalResumeState(state: PersistedSmResumeState): XmppResumeState {
  if (!Array.isArray(state.unhandledOutboundEntries)) {
    throw new TypeError(
      "XEP-0198 unhandledOutboundEntries must be an ordered array",
    );
  }
  const unhandledOutboundEntries = state.unhandledOutboundEntries;
  const maxResumeSeconds = resumeMaxSeconds(state.maxResumeSeconds);
  return {
    previd: state.previd,
    inboundH: state.inboundH,
    outboundH: state.outboundH,
    unhandledOutboundEntries,
    ...(maxResumeSeconds === undefined ? {} : { maxResumeSeconds }),
    ...(state.resource === undefined ? {} : { resource: state.resource }),
  };
}

type ResumeStateConfig = {
  with_resume_state(state: Omit<XmppResumeState, "resource">): void;
};

export function applyResumeStateToWasmConfig(
  config: ResumeStateConfig,
  resumeState: XmppResumeState,
): void {
  const snapshot = canonicalResumeState(resumeState);
  config.with_resume_state({
    previd: snapshot.previd,
    inboundH: snapshot.inboundH,
    outboundH: snapshot.outboundH,
    unhandledOutboundEntries: snapshot.unhandledOutboundEntries,
    ...(snapshot.maxResumeSeconds === undefined
      ? {}
      : { maxResumeSeconds: snapshot.maxResumeSeconds }),
  });
}

type ReconnectSchedulerDeps = {
  isDestroying: () => boolean;
  connect: () => Promise<void>;
  onScheduled: (info: { attempt: number; delayMs: number }) => void;
  /** #1164: the attempt budget ran out — the caller should surface a terminal error state. */
  onExhausted: () => void;
  timeoutScheduler?: TimeoutScheduler;
};

export type TimeoutScheduler = {
  setTimeout: (callback: () => void, delayMs: number) => unknown;
  clearTimeout: (handle: unknown) => void;
};

export const systemTimeoutScheduler: TimeoutScheduler = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
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
  private timer: unknown | null = null;
  private startedAt: number | null = null;

  constructor(private readonly deps: ReconnectSchedulerDeps) {}

  schedule(): void {
    if (this.deps.isDestroying() || this.timer !== null) return;
    if (this.attempt >= MAX_RECONNECT_ATTEMPTS) {
      this.deps.onExhausted();
      return;
    }
    const delay = Math.min(2000 * (2 ** this.attempt), 60000);
    this.attempt += 1;
    this.deps.onScheduled({ attempt: this.attempt, delayMs: delay });
    this.timer = (this.deps.timeoutScheduler ?? systemTimeoutScheduler).setTimeout(() => {
      this.timer = null;
      void this.deps.connect().catch(() => undefined);
    }, delay);
  }

  clearTimer(): void {
    if (this.timer !== null) {
      (this.deps.timeoutScheduler ?? systemTimeoutScheduler).clearTimeout(this.timer);
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
  get_resume_state: () => XmppResumeState | null;
};

export type ResumeStateTeardownStage =
  | "sm-clear"
  | "joined-rooms-clear";

export class ResumeStateTeardownError extends Error {
  constructor(
    readonly failures: ReadonlyArray<{
      stage: ResumeStateTeardownStage;
      cause: unknown;
    }>,
  ) {
    super(
      `Resume-state teardown failed at ${failures.map(({ stage }) => stage).join(", ")}`,
      {
        cause: new AggregateError(
          failures.map(({ cause }) => cause),
          "Resume-state teardown failures",
        ),
      },
    );
    this.name = "ResumeStateTeardownError";
  }
}

/**
 * XEP-0198 resume-state bookkeeping. One complete typed POD snapshot is used
 * for both an in-context reconnect and a full page reload, and is mirrored
 * into `ResumePersistence` only during the page-lifecycle handoff.
 */
export class ResumeStateStore {
  private stateValue: XmppResumeState | null = null;
  private pageLifecycleEpoch = 0;
  private hiddenPageEpoch: number | null = null;
  private pageLifecycleTail: Promise<void> = Promise.resolve();
  private readonly pageLifecycleFailures: unknown[] = [];

  constructor(private readonly persistence: ResumePersistence) {}

  get state(): XmppResumeState | null {
    return this.stateValue;
  }

  get pagehideHandoffActive(): boolean {
    return this.hiddenPageEpoch !== null;
  }

  /** Hydrate the SM state persisted by a prior tab session (one-shot). */
  async consumePersisted(): Promise<XmppResumeState | null> {
    const persisted = committedOrThrow(
      "sm-consume",
      await this.persistence.consumeSm(),
    );
    this.stateValue = persisted ? canonicalResumeState(persisted) : null;
    return this.stateValue;
  }

  /** Drop the in-memory POD state and its persisted copy. */
  async discardState(): Promise<void> {
    this.stateValue = null;
    committedOrThrow("sm-clear", await this.persistence.clearSm());
  }

  /** Reject an unusable native resume candidate without clearing room state. */
  async discardCandidate(): Promise<void> {
    this.stateValue = null;
    const failures: Array<{
      stage: ResumeStateTeardownStage;
      cause: unknown;
    }> = [];
    try {
      committedOrThrow(
        "sm-clear-rejected-candidate",
        await this.persistence.clearSm(),
      );
    } catch (error) {
      failures.push({ stage: "sm-clear", cause: error });
    }
    if (failures.length > 0) throw new ResumeStateTeardownError(failures);
  }

  /** Full teardown: state, persisted SM slot, and retained-room list. */
  async clearAll(): Promise<void> {
    this.stateValue = null;
    const failures: Array<{
      stage: ResumeStateTeardownStage;
      cause: unknown;
    }> = [];
    try {
      committedOrThrow("sm-clear-all", await this.persistence.clearSm());
    } catch (error) {
      failures.push({ stage: "sm-clear", cause: error });
    }
    try {
      this.persistence.clearJoinedRooms();
    } catch (error) {
      failures.push({ stage: "joined-rooms-clear", cause: error });
    }
    if (failures.length > 0) throw new ResumeStateTeardownError(failures);
  }

  /**
   * Capture the typed snapshot from a disconnecting WASM client. Keeps the
   * captured state in this JS context only — the shared per-account
   * persisted SM slot is a pagehide handoff for true tab replacement;
   * writing it during ordinary disconnects would let another live tab
   * claim this same resource while this client still owns the reconnect.
   */
  captureFromDisconnect(source: ResumeStateSource, resource: string): XmppResumeState | null {
    const resumeState = source.get_resume_state();
    this.stateValue = resumeState
      ? canonicalResumeState({ ...resumeState, resource })
      : null;
    return this.stateValue;
  }

  persistForPageHide(
    liveState: XmppResumeState | null,
    resource: string,
    persistJoinedRooms: () => void,
  ): void {
    const state = liveState ? canonicalResumeState(liveState) : this.stateValue;
    const snapshot = state
      ? structuredClone({ ...state, resource })
      : null;
    this.stateValue = snapshot ? structuredClone(snapshot) : null;
    const epoch = ++this.pageLifecycleEpoch;
    this.hiddenPageEpoch = epoch;
    this.enqueuePageLifecycle(async () => {
      // A pageshow that arrived before this operation started invalidates the
      // pagehide epoch; it must not create or publish a successor token.
      if (this.hiddenPageEpoch !== epoch) return;
      const receipt = committedOrThrow(
        "sm-prepare-pagehide",
        await this.persistence.preparePagehideHandoff(snapshot),
      );
      if (this.hiddenPageEpoch === epoch) {
        this.persistence.publishPagehideHandoff(receipt);
        return;
      }
      // pageshow raced the durable transaction. Never publish that reclaimed
      // epoch; cancel its exact token+SM version before later lifecycle work.
      committedOrThrow(
        "sm-cancel-raced-pagehide",
        await this.persistence.reclaimPagehideOwnership(),
      );
    });
    persistJoinedRooms();
  }

  reclaimAfterPageShow(): void {
    this.hiddenPageEpoch = null;
    this.pageLifecycleEpoch += 1;
    this.enqueuePageLifecycle(async () => {
      committedOrThrow(
        "sm-reclaim-pageshow",
        await this.persistence.reclaimPagehideOwnership(),
      );
    });
  }

  async whenPageLifecycleQuiescent(): Promise<void> {
    const failures = new Set<unknown>();
    for (;;) {
      const tail = this.pageLifecycleTail;
      await tail;
      for (const error of this.pageLifecycleFailures.splice(0)) {
        collectQueueFailure(failures, error);
      }
      await Promise.resolve();
      if (tail !== this.pageLifecycleTail) continue;
      if (failures.size > 0) {
        throw new AggregateError(
          [...failures],
          "SM page lifecycle persistence failed",
        );
      }
      return;
    }
  }

  private enqueuePageLifecycle(operation: () => Promise<void>): void {
    const current = this.pageLifecycleTail.then(operation);
    this.pageLifecycleTail = current.then(
      () => undefined,
      (error) => {
        this.pageLifecycleFailures.push(error);
        reportError("storage.write", error, {
          recoverable: false,
          detail: "serialized page lifecycle owner/SM handoff failed",
          storage_area: "sm-resume",
        });
      },
    );
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
  lifecycleId: XmppLifecycleId;
  outboundOwnerHint: () => OutboundOwnerHint;
  acceptOutboundOwner: (activation: OutboundOwnerActivation) => void;
  /**
   * Synchronous fail-closed notification. The client must detach the exact
   * transport generation before this callback returns; physical disposal may
   * continue asynchronously after the authority flight releases.
   */
  onAuthorityLost: (error: OutboundAuthorityChangedError) => void;
};

type NativeQueueOwnership =
  | { kind: "resume-replay"; claim: OutboundClaim }
  | { kind: "fresh-fallback"; claim: OutboundClaim };

const TERMINAL_RETRY_DELAYS_MS = [250, 500, 1_000, 2_000, 5_000] as const;

function isExactQueueDisposedError(error: unknown): boolean {
  return error instanceof DOMException
    && error.name === "AbortError"
    && error.message === "Outbound queue disposed";
}

function outboundQueueDisposedError(): DOMException {
  return new DOMException("Outbound queue disposed", "AbortError");
}

function collectUnexpectedQueueFailures(
  failures: Set<unknown>,
  settled: readonly PromiseSettledResult<unknown>[],
  allow: (error: unknown) => boolean = () => false,
): void {
  for (const result of settled) {
    if (result.status === "rejected" && !allow(result.reason)) {
      collectQueueFailure(failures, result.reason, allow);
    }
  }
}

function collectQueueFailure(
  failures: Set<unknown>,
  error: unknown,
  allow: (error: unknown) => boolean = () => false,
): void {
  if (allow(error)) return;
  if (error instanceof AggregateError) {
    for (const nested of error.errors) {
      collectQueueFailure(failures, nested, allow);
    }
    return;
  }
  failures.add(error);
}

export class NativeResumeAuthorityBlockedError extends DOMException {
  constructor(readonly messageIds: readonly string[]) {
    super(
      `Native resume ownership is held by another live sender: ${messageIds.join(", ")}`,
      "AbortError",
    );
  }
}

export class NativeResumeSnapshotRejectedError extends DOMException {
  constructor(
    readonly terminalIds: readonly string[],
    readonly missingIds: readonly string[],
  ) {
    super(
      "Native resume snapshot references terminal or missing durable rows",
      "InvalidStateError",
    );
  }
}

export class OutboundAuthorityChangedError extends DOMException {
  constructor(
    readonly ownerId: string,
    readonly ownerInstanceId: string,
    readonly ownerGeneration: number,
    readonly authorityEpoch: number,
  ) {
    super("Outbound owner authority changed during revalidation", "AbortError");
  }
}

export type NativeSnapshotReconciliation = {
  terminalIds: string[];
  missingIds: string[];
};

function sameRowIdentity(
  left: OutboundRowIdentity | undefined,
  right: OutboundRowIdentity,
): boolean {
  return !!left
    && left.accountKey === right.accountKey
    && left.messageId === right.messageId
    && left.incarnation === right.incarnation
    && left.payloadDigest === right.payloadDigest;
}

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
  private readonly pendingSendAt = new Map<string, {
    at: number;
    kind: "room" | "dm";
    identity: OutboundRowIdentity;
  }>();
  private readonly durableMessages = new Map<string, PersistedQueuedMessage>();
  private readonly durableIdentities = new Map<string, OutboundRowIdentity>();
  private readonly claims = new Map<string, OutboundClaim>();
  private readonly durableStore: DurableOutboundStore;
  private readonly durableHydration: Promise<void>;
  private owner: OutboundOwnerContext | null = null;
  private connectionGeneration = 0;
  private connectionGenerationBound = false;
  private renewalTimer: ReturnType<typeof setInterval> | null = null;
  private authorityTimer: ReturnType<typeof setInterval> | null = null;
  private authorityFlight: Promise<void> | null = null;
  private authorityDirty = false;
  private authorityEpoch = 0;
  private readonly authorityReasons = new Set<string>();
  private readonly backgroundFailures: unknown[] = [];
  private nativeReconciliationTail: Promise<void> = Promise.resolve();
  private nativeReconciliationPending = 0;
  private terminalTail: Promise<void> = Promise.resolve();
  private terminalRetryTimer: ReturnType<typeof setTimeout> | null = null;
  private terminalRetryWake: (() => void) | null = null;
  private disposePromise: Promise<void> | null = null;
  private disposalOwner: OutboundOwnerContext | null = null;
  private disposalConnectionGeneration: number | null = null;
  private revisionChannel: BroadcastChannel | null = null;
  private onlineListener: (() => void) | null = null;
  private visibilityListener: (() => void) | null = null;
  private pageShowListener: (() => void) | null = null;
  private disposed = false;
  private readonly leaseWakeTimers = new Map<string, ReturnType<typeof setTimeout>>();
  private directFlushPromise: Promise<void> | null = null;
  private readonly roomFlushes = new Map<string, Promise<void>>();

  constructor(private readonly deps: OfflineSendQueueDeps) {
    this.durableStore = deps.durableStore
      ?? new IndexedDbDurableOutboundStore();
    this.durableHydration = this.hydrateDurableMessages();
  }

  beginConnectionGeneration(generation?: number): number {
    if (this.disposed) throw outboundQueueDisposedError();
    if (this.nativeReconciliationPending > 0) {
      throw new DOMException(
        "A predecessor native snapshot is still reconciling",
        "AbortError",
      );
    }
    this.connectionGeneration = generation ?? this.connectionGeneration + 1;
    this.connectionGenerationBound = true;
    return this.connectionGeneration;
  }

  ready(): Promise<void> {
    return this.durableHydration;
  }

  /**
   * Deterministic lifecycle barrier for owners that must not infer durable
   * completion from an arbitrary number of microtasks. It observes newly
   * scheduled reconciliation, terminal, and lane work until one complete
   * pass sees the same settled set.
   */
  async whenQuiescent(): Promise<void> {
    const failures = new Set<unknown>();
    const observedWork = new Set<Promise<unknown>>();
    const allow = (error: unknown) => (
      this.disposed && isExactQueueDisposedError(error)
    );
    for (;;) {
      const hydration = this.durableHydration;
      const nativeReconciliation = this.nativeReconciliationTail;
      const terminal = this.terminalTail;
      const authority = this.authorityFlight;
      const direct = this.directFlushPromise;
      const rooms = [...this.roomFlushes.values()];
      const work: Promise<unknown>[] = [
        hydration,
        nativeReconciliation,
        terminal,
        ...(authority ? [authority] : []),
        ...(direct ? [direct] : []),
        ...rooms,
      ];
      const settled = await Promise.allSettled(work);
      const newlyObserved = settled.filter((_, index) => {
        const promise = work[index]!;
        if (observedWork.has(promise)) return false;
        observedWork.add(promise);
        return true;
      });
      collectUnexpectedQueueFailures(failures, newlyObserved, allow);
      for (const error of this.backgroundFailures.splice(0)) {
        if (error instanceof OutboundAuthorityChangedError) continue;
        collectQueueFailure(failures, error, allow);
      }
      await Promise.resolve();
      if (
        hydration === this.durableHydration
        && nativeReconciliation === this.nativeReconciliationTail
        && this.nativeReconciliationPending === 0
        && terminal === this.terminalTail
        && authority === this.authorityFlight
        && direct === this.directFlushPromise
        && rooms.length === this.roomFlushes.size
        && rooms.every((room) => [...this.roomFlushes.values()].includes(room))
      ) {
        if (failures.size > 0) {
          throw new AggregateError(
            [...failures],
            "Outbound queue quiescence failed",
          );
        }
        return;
      }
    }
  }

  private async hydrateDurableMessages(): Promise<void> {
    const activation = committedOrThrow(
      "activate-outbound-owner",
      await this.durableStore.claimOwner(
        this.deps.queueScope(),
        this.deps.outboundOwnerHint(),
      ),
    );
    if (this.disposed) return;
    this.installOwner(activation);
    await this.requestAuthority("hydrate");
    this.startAuthorityObservers();
  }

  private startAuthorityObservers(): void {
    if (this.disposed) return;
    this.authorityTimer = setInterval(() => {
      this.observeAuthorityAndWake("periodic");
    }, 5_000);
    (this.authorityTimer as { unref?: () => void }).unref?.();

    if (typeof window === "undefined") return;
    if (typeof BroadcastChannel !== "undefined") {
      this.revisionChannel = new BroadcastChannel("waddle.chat.xmpp-runtime-revision.v1");
      this.revisionChannel.onmessage = (event: MessageEvent<unknown>) => {
        const hint = event.data as { accountKey?: unknown; revision?: unknown } | null;
        if (
          hint?.accountKey === this.deps.queueScope()
          && typeof hint.revision === "number"
        ) {
          this.observeAuthorityAndWake("revision-hint");
        }
      };
    }
    this.onlineListener = () => {
      this.observeAuthorityAndWake("online");
    };
    this.pageShowListener = () => {
      this.observeAuthorityAndWake("pageshow");
    };
    this.visibilityListener = () => {
      if (document.visibilityState === "visible") {
        this.observeAuthorityAndWake("visibilitychange");
      }
    };
    if (typeof window.addEventListener === "function") {
      window.addEventListener("online", this.onlineListener);
      window.addEventListener("pageshow", this.pageShowListener);
    }
    if (
      typeof document !== "undefined"
      && typeof document.addEventListener === "function"
    ) {
      document.addEventListener("visibilitychange", this.visibilityListener);
    }
  }

  private async revalidateOwner(): Promise<void> {
    if (this.disposed) throw outboundQueueDisposedError();
    const owner = this.owner;
    if (!owner) throw new Error("Outbound owner is not active");
    const renewed = committedOrThrow(
      "renew-outbound-owner",
      await this.durableStore.renewOwner(owner),
    );
    if (this.disposed) throw outboundQueueDisposedError();
    if (renewed) return;
    this.clearEphemeralAuthority();
    this.owner = null;
    this.connectionGenerationBound = false;
    const changed = new OutboundAuthorityChangedError(
      owner.ownerId,
      owner.ownerInstanceId,
      owner.ownerGeneration,
      owner.authorityEpoch,
    );
    // This callback is deliberately synchronous: no owner-dependent work may
    // continue while the old socket is still reachable as the live transport.
    this.deps.onAuthorityLost(changed);
    await this.reactivateOwner();
    throw changed;
  }

  private async reactivateOwner(): Promise<void> {
    if (this.disposed) throw outboundQueueDisposedError();
    const activation = committedOrThrow(
      "reactivate-outbound-owner",
      await this.durableStore.claimOwner(
        this.deps.queueScope(),
        this.deps.outboundOwnerHint(),
      ),
    );
    if (this.disposed) throw outboundQueueDisposedError();
    this.installOwner(activation);
  }

  private installOwner(activation: OutboundOwnerActivation): void {
    if (this.disposed) return;
    const owner = activation.fence;
    const previous = this.owner;
    if (
      previous
      && (
        previous.ownerId !== owner.ownerId
        || previous.ownerInstanceId !== owner.ownerInstanceId
        || previous.ownerGeneration !== owner.ownerGeneration
        || previous.authorityEpoch !== owner.authorityEpoch
      )
    ) {
      this.clearEphemeralAuthority();
    }
    this.owner = owner;
    this.deps.acceptOutboundOwner(activation);
  }

  private observeAuthorityAndWake(reason: string): void {
    void this.requestAuthority(reason)
      .then(() => this.flushConnectedLanes())
      .catch((error) => {
        this.backgroundFailures.push(error);
        this.reportAuthorityError(`authority (${reason})`, error);
      });
  }

  private async flushConnectedLanes(): Promise<void> {
    if (!this.deps.canUseConnectedSession()) return;
    await this.flushDirect();
    for (const message of this.durableMessages.values()) {
      const lane = outboundLane(message);
      if (lane.kind !== "room" || !this.deps.roomIsReady(lane.roomJid)) continue;
      await this.flushRoom(lane.roomJid);
    }
  }

  /**
   * The sole owner-authority serialization point. Every browser wake and each
   * owner-dependent pre-send joins this flight, renews the exact fence first,
   * and only then reconciles durable terminal and lane state.
   */
  private requestAuthority(reason: string): Promise<void> {
    if (this.disposed) return Promise.reject(outboundQueueDisposedError());
    this.authorityDirty = true;
    this.authorityReasons.add(reason);
    if (this.authorityFlight) return this.authorityFlight;
    const operation = (async () => {
      while (this.authorityDirty && !this.disposed) {
        this.authorityDirty = false;
        this.authorityReasons.clear();
        this.authorityEpoch += 1;
        await this.revalidateOwner();
        await this.drainTerminalIntents();
        await this.reconcileAuthority();
      }
    })();
    const tracked = operation.finally(() => {
      if (this.authorityFlight === tracked) this.authorityFlight = null;
      if (this.authorityDirty && !this.disposed) {
        this.observeAuthorityAndWake("coalesced-dirty");
      }
    });
    this.authorityFlight = tracked;
    return tracked;
  }

  private requireOwner(): OutboundOwnerContext {
    const owner = this.owner;
    if (!owner) throw new Error("Outbound owner is not active");
    return owner;
  }

  private requireConnectionGeneration(): number {
    if (!this.connectionGenerationBound) {
      throw new DOMException(
        "Outbound owner is not bound to a live connection generation",
        "AbortError",
      );
    }
    return this.connectionGeneration;
  }

  private async revalidateBeforeOwnerMutation(reason: string): Promise<OutboundOwnerContext> {
    await this.requestAuthority(reason);
    const owner = this.requireOwner();
    if (this.disposed) throw outboundQueueDisposedError();
    return owner;
  }

  private async reconcileAuthority(): Promise<void> {
    if (this.disposed) return;
    const scan = committedOrThrow(
      "scan-outbound-authority",
      await this.durableStore.scanAndPrune(
        this.deps.queueScope(),
        Date.now() - QUEUE_TTL_MS,
      ),
    );
    if (this.disposed) return;
    const authoritativeById = new Map(
      scan.entries.map((entry) => [entry.identity.messageId, entry] as const),
    );
    for (const id of [...this.durableMessages.keys()]) {
      if (authoritativeById.has(id)) continue;
      this.durableMessages.delete(id);
      this.durableIdentities.delete(id);
      this.clearEphemeralForId(id);
      removeQueuedMessage(this.deps.queueScope(), id);
    }
    for (const identity of scan.pruned) {
      if (!authoritativeById.has(identity.messageId)) {
        removeQueuedMessage(this.deps.queueScope(), identity.messageId);
      }
    }
    for (const entry of scan.entries) {
      const previousIdentity = this.durableIdentities.get(entry.identity.messageId);
      if (previousIdentity && !sameRowIdentity(previousIdentity, entry.identity)) {
        this.clearEphemeralForId(entry.identity.messageId);
      }
      this.durableMessages.set(entry.identity.messageId, entry.message);
      this.durableIdentities.set(entry.identity.messageId, entry.identity);
      enqueueQueuedMessage(this.deps.queueScope(), entry.message);
    }
    this.emitQueueDepth();
  }

  private publishRevisionHint(): void {
    if (!this.revisionChannel) return;
    const channel = this.revisionChannel;
    void this.durableStore.revision(this.deps.queueScope())
      .then((outcome) => {
        if (outcome.kind === "failed") {
          this.reportAuthorityError("revision hint read", outcome.cause);
          return;
        }
        try {
          channel.postMessage({
            accountKey: this.deps.queueScope(),
            revision: outcome.value,
          });
        } catch (error) {
          this.reportAuthorityError("revision hint publish", error);
        }
      })
      .catch((error) => {
        this.reportAuthorityError("revision hint publish", error);
      });
  }

  private reportAuthorityError(detail: string, error: unknown): void {
    reportError("storage.write", error, {
      recoverable: false,
      detail: `outbound ${detail} failed`,
      storage_area: "outbound-queue",
    });
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
    const identity = this.durableIdentities.get(id);
    if (!identity) {
      throw new Error("Outbound attempt has no durable row identity");
    }
    const wasInflight = this.inflightQueuedIds.has(id);
    this.inflightQueuedIds.add(id);
    if (claim) this.trackClaim(id, claim);
    this.pendingSendAt.set(id, {
      at: performance.now(),
      kind,
      identity: { ...identity },
    });
    if (!wasInflight) this.emitQueueDepth();
  }

  /** A rejected or null send did not transfer responsibility to XEP-0198. */
  async rollbackAttempt(id: string): Promise<void> {
    await this.durableHydration;
    const claim = this.claims.get(id);
    const identity = this.durableIdentities.get(id);
    if (claim && identity) {
      const released = committedOrThrow(
        "release",
        await this.durableStore.release(identity, claim),
      );
      if (released.kind === "fenced") {
        throw new DOMException(
          "Outbound owner fenced before claim release",
          "AbortError",
        );
      }
      if (released.kind === "missing") {
        throw new Error("Outbound queue claim changed before release");
      }
      this.untrackClaim(id);
      this.publishRevisionHint();
    }
    const wasInflight = this.inflightQueuedIds.delete(id);
    this.pendingSendAt.delete(id);
    if (wasInflight) {
      this.deps.events.emit("queuedMessageStatus", id, "queued");
      this.emitQueueDepth();
    }
  }

  /**
   * Reconcile one native handle generation against its exact typed XEP-0198
   * snapshot. A null snapshot retires every owned claim; an absent id retires
   * that id even when its previous phase was fresh fallback.
   */
  async reconcileNativeSnapshot(
    generation: number,
    state: XmppResumeState | null | undefined,
    phase: "resume-replay" | "fresh-fallback" = "resume-replay",
  ): Promise<NativeSnapshotReconciliation> {
    return this.serializeNativeReconciliation(async () => {
      await this.durableHydration;
      this.assertConnectionGeneration(generation);
      await this.requestAuthority("native-snapshot");
      this.assertConnectionGeneration(generation);
      return this.serializeTerminal(async () => {
        this.assertConnectionGeneration(generation);
        const owner = this.requireOwner();
        return this.reconcileNativeSnapshotForOwner(
          owner,
          generation,
          state,
          phase,
          false,
        );
      });
    });
  }

  /**
   * Final teardown reconciliation is intentionally authority-static. The
   * captured fence is the only executor it may use: no renewal, owner claim,
   * observer wake, or successor installation is permitted after final
   * disposal begins.
   */
  async reconcileFinalNativeSnapshot(
    generation: number,
    state: XmppResumeState | null | undefined,
  ): Promise<NativeSnapshotReconciliation> {
    if (!this.disposed) {
      throw new DOMException(
        "Final native reconciliation requires terminal queue state",
        "InvalidStateError",
      );
    }
    if (this.disposalConnectionGeneration === null) {
      return { terminalIds: [], missingIds: [] };
    }
    if (generation !== this.disposalConnectionGeneration) {
      throw new DOMException(
        "Native snapshot belongs to a stale connection generation",
        "AbortError",
      );
    }
    const owner = this.disposalOwner;
    if (!owner) return { terminalIds: [], missingIds: [] };
    return this.serializeNativeReconciliation(() => (
      this.serializeTerminal(() => this.reconcileNativeSnapshotForOwner(
        owner,
        generation,
        state,
        "resume-replay",
        true,
      ))
    ));
  }

  private async reconcileNativeSnapshotForOwner(
    owner: OutboundOwnerContext,
    generation: number,
    state: XmppResumeState | null | undefined,
    phase: "resume-replay" | "fresh-fallback",
    finalDisposal: boolean,
  ): Promise<NativeSnapshotReconciliation> {
    const ids = state === null || state === undefined
      ? null
      : (state.unhandledOutboundEntries ?? [])
          .map((entry) => messageStanzaIdFromResumeStanza(entry.stanza))
          .filter((id): id is string => !!id);
    const reconciliation = committedOrThrow(
      "reconcile-native-snapshot",
      await this.durableStore.reconcileResumeClaims(
        owner,
        generation,
        ids,
        phase,
      ),
    );
    if (!finalDisposal) this.assertConnectionGeneration(generation);
    if (reconciliation.kind === "fenced") {
      throw new DOMException(
        "Outbound owner fenced during native reconciliation",
        "AbortError",
      );
    }
    if (reconciliation.blockedIds.length > 0) {
      throw new NativeResumeAuthorityBlockedError(reconciliation.blockedIds);
    }
    if (
      reconciliation.terminalIds.length > 0
      || reconciliation.missingIds.length > 0
    ) {
      throw new NativeResumeSnapshotRejectedError(
        reconciliation.terminalIds,
        reconciliation.missingIds,
      );
    }
    if (!finalDisposal) {
      for (const id of reconciliation.releasedIds) {
        this.nativeQueueOwnership.delete(id);
        this.inflightQueuedIds.delete(id);
        this.untrackClaim(id);
        this.deps.events.emit("queuedMessageStatus", id, "queued");
      }
      for (const { messageId, claim } of reconciliation.claims) {
        this.inflightQueuedIds.add(messageId);
        this.nativeQueueOwnership.set(messageId, {
          kind: claim.phase === "fresh-fallback" ? "fresh-fallback" : "resume-replay",
          claim,
        });
        this.trackClaim(messageId, claim);
      }
      this.publishRevisionHint();
      this.emitQueueDepth();
    }
    return { terminalIds: [], missingIds: [] };
  }

  private assertConnectionGeneration(generation: number): void {
    if (
      !this.connectionGenerationBound
      || generation !== this.connectionGeneration
    ) {
      throw new DOMException(
        "Native snapshot belongs to a stale connection generation",
        "AbortError",
      );
    }
  }

  private serializeNativeReconciliation<T>(
    operation: () => Promise<T>,
  ): Promise<T> {
    this.nativeReconciliationPending += 1;
    const result = this.nativeReconciliationTail.then(operation);
    const tracked = result.finally(() => {
      this.nativeReconciliationPending -= 1;
    });
    this.nativeReconciliationTail = tracked.then(
      () => undefined,
      () => undefined,
    );
    return tracked;
  }

  async reconcileFreshSession(
    generation: number,
    liveState: XmppResumeState | null | undefined,
  ): Promise<NativeSnapshotReconciliation> {
    return this.reconcileNativeSnapshot(generation, liveState, "fresh-fallback");
  }

  async handleAck(id: string, generation = this.connectionGeneration): Promise<void> {
    await this.durableHydration;
    return this.serializeTerminal(() => this.recordAndApplyTerminal(id, generation, "ack"));
  }

  async handleFailed(id: string, generation = this.connectionGeneration): Promise<void> {
    await this.durableHydration;
    return this.serializeTerminal(() => (
      this.recordAndApplyTerminal(id, generation, "native-failure")
    ));
  }

  async discardNonRetryable(
    id: string,
    generation = this.connectionGeneration,
  ): Promise<void> {
    await this.durableHydration;
    return this.serializeTerminal(() => (
      this.recordAndApplyTerminal(id, generation, "nonretryable-delete")
    ));
  }

  private serializeTerminal<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.terminalTail.then(operation);
    this.terminalTail = result.then(
      () => undefined,
      (error) => {
        this.backgroundFailures.push(error);
        this.reportAuthorityError("terminal worker", error);
      },
    );
    return result;
  }

  private async recordAndApplyTerminal(
    id: string,
    generation: number,
    kind: "ack" | "native-failure" | "nonretryable-delete",
  ): Promise<void> {
    const identity = this.durableIdentities.get(id);
    const ownership = this.nativeQueueOwnership.get(id);
    const claim = ownership?.claim ?? this.claims.get(id);
    if (
      !identity
      || !claim
      || claim.connectionGeneration !== generation
      || claim.rowIncarnation !== identity.incarnation
      || claim.payloadDigest !== identity.payloadDigest
    ) return;

    const recorded = await this.retryTerminalOutcome(
      `record-terminal-${kind}`,
      () => this.durableStore.recordTerminal(identity, kind, claim),
    );
    if (recorded.kind !== "recorded") return;
    await this.applyTerminalIntent(recorded.intent);
    this.publishRevisionHint();
  }

  private async drainTerminalIntents(
    executor?: OutboundOwnerContext,
    allowDuringFinalDisposal = false,
  ): Promise<void> {
    const intents = await this.retryTerminalOutcome(
      "list-terminal-intents",
      () => this.durableStore.listTerminal(this.deps.queueScope()),
      allowDuringFinalDisposal,
    );
    for (const intent of intents) {
      await this.applyTerminalIntent(
        intent,
        executor,
        allowDuringFinalDisposal,
      );
    }
  }

  private async applyTerminalIntent(
    intent: OutboundTerminalIntent,
    executor?: OutboundOwnerContext,
    allowDuringFinalDisposal = false,
  ): Promise<void> {
    const owner = executor
      ?? (this.disposed ? this.disposalOwner : this.requireOwner());
    if (!owner) throw outboundQueueDisposedError();
    const applied = await this.retryTerminalOutcome(
      "apply-terminal-intent",
      () => this.durableStore.applyTerminal(owner, intent),
      allowDuringFinalDisposal,
    );
    await this.applyTerminalResult(applied, allowDuringFinalDisposal);
  }

  private async retryTerminalOutcome<T>(
    operation: string,
    run: () => Promise<DurableOutcome<T>>,
    allowDuringFinalDisposal = false,
  ): Promise<T> {
    let attempt = 0;
    for (;;) {
      if (this.disposed && !allowDuringFinalDisposal) {
        throw outboundQueueDisposedError();
      }
      const outcome = await run();
      if (outcome.kind === "committed") return outcome.value;
      if (this.disposed && !allowDuringFinalDisposal) {
        throw outboundQueueDisposedError();
      }
      this.reportAuthorityError(`${operation} retry`, outcome.cause);
      const delay = TERMINAL_RETRY_DELAYS_MS[
        Math.min(attempt, TERMINAL_RETRY_DELAYS_MS.length - 1)
      ]!;
      attempt += 1;
      await new Promise<void>((resolve) => {
        const finish = () => {
          if (this.terminalRetryTimer) clearTimeout(this.terminalRetryTimer);
          this.terminalRetryTimer = null;
          this.terminalRetryWake = null;
          resolve();
        };
        this.terminalRetryWake = finish;
        this.terminalRetryTimer = setTimeout(finish, delay);
        (this.terminalRetryTimer as { unref?: () => void }).unref?.();
      });
    }
  }

  private async applyTerminalResult(
    result: OutboundTerminalApplyResult,
    allowFenced = false,
  ): Promise<void> {
    if (result.kind === "fenced") {
      if (allowFenced) return;
      throw new DOMException(
        "Outbound owner fenced during terminal application",
        "AbortError",
      );
    }
    if (result.kind === "missing" || result.kind === "stale") return;
    const id = result.identity.messageId;
    const localIdentity = this.durableIdentities.get(id);
    const identityMatches = sameRowIdentity(localIdentity, result.identity);
    const message = identityMatches ? this.durableMessages.get(id) : undefined;
    if (result.kind === "fallback") {
      if (!identityMatches) {
        await this.reconcileAuthority();
        return;
      }
      this.inflightQueuedIds.delete(id);
      this.nativeQueueOwnership.set(id, { kind: "fresh-fallback", claim: result.claim });
      this.trackClaim(id, result.claim);
      this.deps.events.emit("queuedMessageStatus", id, "sending");
      this.emitQueueDepth();
      return;
    }

    const pending = this.pendingSendAt.get(id);
    const exactPending = pending && sameRowIdentity(pending.identity, result.identity)
      ? pending
      : undefined;
    if (identityMatches) this.clearEphemeralForId(id);
    // Re-read authority before touching the presentation projection. Another
    // tab may already have reused this stanza id with a new incarnation.
    await this.reconcileAuthority();
    if (result.kind === "acked") {
      this.deps.events.emit("messageAck", id);
      if (exactPending) {
        this.deps.events.emitSafe("messageAcked", id, {
          kind: exactPending.kind,
          latencyMs: performance.now() - exactPending.at,
        });
      }
    } else {
      this.deps.events.emit("messageDeliveryFailure", id);
      if (exactPending) {
        this.deps.events.emitSafe("messageDeliveryFailed", id, {
          kind: exactPending.kind,
        });
      }
      if (result.kind === "released") {
        this.deps.events.emit("queuedMessageStatus", id, "queued");
      }
    }
    this.emitQueueDepth();
    if (message) this.wakeLane(outboundLane(message));
  }

  async queueRoomMessage(roomJid: string, body: string, opts: SendGroupMessageOptions): Promise<OutboundSendResult> {
    await this.durableHydration;
    const queuedId = opts.id ?? crypto.randomUUID();
    const lane = roomOutboundLane(roomJid);
    const message: PersistedQueuedMessage = {
      kind: "room",
      id: queuedId,
      createdAt: new Date().toISOString(),
      roomJid: lane.roomJid,
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
    const persisted = committedOrThrow(
      "enqueue-room",
      await this.durableStore.persistReady(this.deps.queueScope(), message),
    );
    if (persisted.kind === "conflict") {
      throw new DOMException("Outbound stanza id was reused with different payload", "ConstraintError");
    }
    this.durableMessages.set(message.id, persisted.entry.message);
    this.durableIdentities.set(message.id, persisted.entry.identity);
    enqueueQueuedMessage(this.deps.queueScope(), persisted.entry.message);
    this.publishRevisionHint();
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
    const persisted = committedOrThrow(
      "enqueue-direct",
      await this.durableStore.persistReady(this.deps.queueScope(), message),
    );
    if (persisted.kind === "conflict") {
      throw new DOMException("Outbound stanza id was reused with different payload", "ConstraintError");
    }
    this.durableMessages.set(message.id, persisted.entry.message);
    this.durableIdentities.set(message.id, persisted.entry.identity);
    enqueueQueuedMessage(this.deps.queueScope(), persisted.entry.message);
    this.publishRevisionHint();
    this.deps.events.emit("queuedMessageStatus", queuedId, "queued");
    this.noteQueuedMessage();
    this.deps.events.emitSafe("sendEnqueued", { kind: "dm", reason: this.deps.enqueueReason() });
    this.emitQueueDepth();
    return { id: queuedId, state: "queued" };
  }

  /** Persist an optimistic live room send so a crash before the ack replays it. */
  async persistPendingRoomSend(
    roomJid: string,
    body: string,
    opts: SendGroupMessageOptions & { id: string },
  ): Promise<PendingSendReservation> {
    await this.durableHydration;
    const owner = await this.revalidateBeforeOwnerMutation("pre-send-room");
    const lane = roomOutboundLane(roomJid);
    const message: PersistedQueuedMessage = {
      kind: "room",
      id: opts.id,
      createdAt: new Date().toISOString(),
      roomJid: lane.roomJid,
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
    const claim = createOutboundClaim(owner, this.requireConnectionGeneration(), "sending");
    const persisted = committedOrThrow(
      "persist-live-room",
      await this.durableStore.persistAndClaimLaneHead(
        this.deps.queueScope(),
        message,
        claim,
      ),
    );
    if (persisted.kind !== "claimed" && persisted.kind !== "queued") {
      throw new DOMException(
        persisted.kind === "conflict"
          ? "Outbound stanza id was reused with different payload"
          : `Outbound stanza is ${persisted.kind}`,
        persisted.kind === "conflict" ? "ConstraintError" : "AbortError",
      );
    }
    this.durableMessages.set(message.id, persisted.entry.message);
    this.durableIdentities.set(message.id, persisted.entry.identity);
    enqueueQueuedMessage(this.deps.queueScope(), persisted.entry.message);
    this.publishRevisionHint();
    if (persisted.kind === "queued") {
      this.deps.events.emit("queuedMessageStatus", message.id, "queued");
      this.noteQueuedMessage();
      this.deps.events.emitSafe("sendEnqueued", {
        kind: "room",
        reason: this.deps.enqueueReason(),
      });
      if (
        persisted.blocker.state === "claimed"
        && persisted.blocker.leaseUntil !== undefined
      ) {
        this.scheduleLaneWake(outboundLane(message), persisted.blocker.leaseUntil);
      } else {
        this.wakeLane(outboundLane(message));
      }
      this.emitQueueDepth();
      return { kind: "queued" };
    }
    this.trackClaim(opts.id, persisted.claim);
    this.emitQueueDepth();
    return { kind: "claimed", claim: persisted.claim };
  }

  /** Persist an optimistic live DM send so a crash before the ack replays it. */
  async persistPendingDirectSend(
    peerJid: string,
    body: string,
    opts: SendDirectMessageOptions & { id: string },
  ): Promise<PendingSendReservation> {
    await this.durableHydration;
    const owner = await this.revalidateBeforeOwnerMutation("pre-send-direct");
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
    const claim = createOutboundClaim(owner, this.requireConnectionGeneration(), "sending");
    const persisted = committedOrThrow(
      "persist-live-direct",
      await this.durableStore.persistAndClaimLaneHead(
        this.deps.queueScope(),
        message,
        claim,
      ),
    );
    if (persisted.kind !== "claimed" && persisted.kind !== "queued") {
      throw new DOMException(
        persisted.kind === "conflict"
          ? "Outbound stanza id was reused with different payload"
          : `Outbound stanza is ${persisted.kind}`,
        persisted.kind === "conflict" ? "ConstraintError" : "AbortError",
      );
    }
    this.durableMessages.set(message.id, persisted.entry.message);
    this.durableIdentities.set(message.id, persisted.entry.identity);
    enqueueQueuedMessage(this.deps.queueScope(), persisted.entry.message);
    this.publishRevisionHint();
    if (persisted.kind === "queued") {
      this.deps.events.emit("queuedMessageStatus", message.id, "queued");
      this.noteQueuedMessage();
      this.deps.events.emitSafe("sendEnqueued", {
        kind: "dm",
        reason: this.deps.enqueueReason(),
      });
      if (
        persisted.blocker.state === "claimed"
        && persisted.blocker.leaseUntil !== undefined
      ) {
        this.scheduleLaneWake(outboundLane(message), persisted.blocker.leaseUntil);
      } else {
        this.wakeLane(outboundLane(message));
      }
      this.emitQueueDepth();
      return { kind: "queued" };
    }
    this.trackClaim(opts.id, persisted.claim);
    this.emitQueueDepth();
    return { kind: "claimed", claim: persisted.claim };
  }

  async flushDirect(): Promise<void | undefined> {
    await this.durableHydration;
    if (this.directFlushPromise) return this.directFlushPromise;
    if (!this.deps.canUseConnectedSession()) return;
    const promise = (async () => {
      await this.terminalTail;
      while (!this.disposed && this.deps.canUseConnectedSession()) {
        const owner = await this.revalidateBeforeOwnerMutation("pre-send-direct-flush");
        const claimResult = committedOrThrow(
          "claim-direct-head",
          await this.durableStore.claimHead(
            this.deps.queueScope(),
            { kind: "direct" },
            createOutboundClaim(owner, this.requireConnectionGeneration(), "sending"),
          ),
        );
        if (claimResult.kind === "missing") break;
        if (claimResult.kind === "fenced") {
          throw new DOMException(
            "Outbound owner fenced before direct lane claim",
            "AbortError",
          );
        }
        if (claimResult.kind === "terminal") {
          await this.serializeTerminal(() => this.drainTerminalIntents());
          continue;
        }
        if (claimResult.kind === "busy") {
          this.scheduleLaneWake({ kind: "direct" }, claimResult.leaseUntil);
          break;
        }
        const entry = claimResult.entry.message;
        if (entry.kind !== "dm") throw new Error("Direct lane returned a room message");
        this.durableMessages.set(entry.id, entry);
        this.durableIdentities.set(entry.id, claimResult.entry.identity);
        this.publishRevisionHint();
        this.deps.events.emit("queuedMessageStatus", entry.id, "sending");
        this.beginAttempt(entry.id, "dm", claimResult.claim);
        let messageId: string | null;
        try {
          messageId = await this.deps.sendDirect(entry.mucPm ? entry.peerJid : barePeerJid(entry.peerJid), entry.body, { ...(entry.markup?.length ? { markup: entry.markup } : {}), ...(entry.references?.length ? { references: entry.references } : {}), ...(entry.files?.length ? { files: entry.files } : {}), ...(entry.replyTo ? { replyTo: entry.replyTo } : {}), ...(entry.threadId ? { threadId: entry.threadId } : {}), ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}), ...(entry.mucPm ? { mucPm: true } : {}), id: entry.id });
        } catch (error) {
          if (isNonRetryableWasmSendFailure(error)) {
            await this.discardNonRetryable(entry.id);
            continue;
          }
          await this.rollbackAttempt(entry.id);
          throw error;
        }
        if (!messageId) {
          await this.rollbackAttempt(entry.id);
          break;
        } else if (messageId !== entry.id) {
          await this.rollbackAttempt(entry.id);
          throw new Error("XMPP send returned a different stanza id");
        }
        break;
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
    const lane = roomOutboundLane(roomJid);
    const canonicalRoomJid = lane.roomJid;
    const inflight = this.roomFlushes.get(canonicalRoomJid);
    if (inflight) return inflight;
    if (!this.deps.roomIsReady(canonicalRoomJid)) return;
    const promise = (async () => {
      await this.terminalTail;
      while (!this.disposed && this.deps.roomIsReady(canonicalRoomJid)) {
        const owner = await this.revalidateBeforeOwnerMutation("pre-send-room-flush");
        const claimResult = committedOrThrow(
          "claim-room-head",
          await this.durableStore.claimHead(
            this.deps.queueScope(),
            lane,
            createOutboundClaim(owner, this.requireConnectionGeneration(), "sending"),
          ),
        );
        if (claimResult.kind === "missing") break;
        if (claimResult.kind === "fenced") {
          throw new DOMException(
            "Outbound owner fenced before room lane claim",
            "AbortError",
          );
        }
        if (claimResult.kind === "terminal") {
          await this.serializeTerminal(() => this.drainTerminalIntents());
          continue;
        }
        if (claimResult.kind === "busy") {
          this.scheduleLaneWake(lane, claimResult.leaseUntil);
          break;
        }
        const entry = claimResult.entry.message;
        if (entry.kind !== "room") throw new Error("Room lane returned a direct message");
        this.durableMessages.set(entry.id, entry);
        this.durableIdentities.set(entry.id, claimResult.entry.identity);
        this.publishRevisionHint();
        this.deps.events.emit("queuedMessageStatus", entry.id, "sending");
        this.beginAttempt(entry.id, "room", claimResult.claim);
        let messageId: string | null;
        try {
          messageId = await this.deps.sendRoom(canonicalRoomJid, entry.body, { ...(entry.markup?.length ? { markup: entry.markup } : {}), ...(entry.references?.length ? { references: entry.references } : {}), mentionJidsByNick: { ...(entry.mentionJidsByNick ?? {}), ...this.deps.roomMemberJids(canonicalRoomJid) }, ...(entry.files?.length ? { files: entry.files } : {}), ...(entry.replyTo ? { replyTo: entry.replyTo } : {}), ...(entry.threadId ? { threadId: entry.threadId } : {}), ...(entry.parentThreadId ? { parentThreadId: entry.parentThreadId } : {}), ...(entry.threadCreate ? { threadCreate: entry.threadCreate } : {}), ...(entry.threadReply ? { threadReply: entry.threadReply } : {}), id: entry.id });
        } catch (error) {
          if (isNonRetryableWasmSendFailure(error)) {
            await this.discardNonRetryable(entry.id);
            continue;
          }
          await this.rollbackAttempt(entry.id);
          throw error;
        }
        if (!messageId) {
          await this.rollbackAttempt(entry.id);
          break;
        } else if (messageId !== entry.id) {
          await this.rollbackAttempt(entry.id);
          throw new Error("XMPP send returned a different stanza id");
        }
        break;
      }
    })();
    const trackedPromise = promise.finally(() => {
      if (this.roomFlushes.get(canonicalRoomJid) === trackedPromise) {
        this.roomFlushes.delete(canonicalRoomJid);
      }
    });
    this.roomFlushes.set(canonicalRoomJid, trackedPromise);
    return trackedPromise;
  }

  private scheduleLaneWake(lane: ReturnType<typeof outboundLane>, leaseUntil: number): void {
    const canonicalLane = lane.kind === "room"
      ? roomOutboundLane(lane.roomJid)
      : lane;
    const key = canonicalLane.kind === "direct"
      ? "direct"
      : `room:${canonicalLane.roomJid}`;
    const current = this.leaseWakeTimers.get(key);
    if (current) clearTimeout(current);
    const timer = setTimeout(() => {
      this.leaseWakeTimers.delete(key);
      this.wakeLane(canonicalLane);
    }, Math.max(0, leaseUntil - Date.now()) + 1);
    (timer as { unref?: () => void }).unref?.();
    this.leaseWakeTimers.set(key, timer);
  }

  private wakeLane(lane: ReturnType<typeof outboundLane>): void {
    const canonicalLane = lane.kind === "room"
      ? roomOutboundLane(lane.roomJid)
      : lane;
    queueMicrotask(() => {
      if (this.disposed) return;
      const flush = canonicalLane.kind === "direct"
        ? this.flushDirect()
        : this.flushRoom(canonicalLane.roomJid);
      void Promise.resolve(flush).catch((error) => {
        this.backgroundFailures.push(error);
        this.reportAuthorityError("lane wake", error);
      });
    });
  }

  private trackClaim(id: string, claim: OutboundClaim): void {
    if (this.disposed) return;
    this.claims.set(id, claim);
    if (this.renewalTimer) return;
    this.renewalTimer = setInterval(() => {
      void this.renewClaims().catch((error) => {
        this.backgroundFailures.push(error);
        reportError("storage.write", error, {
          recoverable: false,
          detail: "outbound claim renewal failed",
          storage_area: "outbound-queue",
        });
      });
    }, Math.floor(OUTBOUND_CLAIM_LEASE_MS / 3));
    (this.renewalTimer as { unref?: () => void }).unref?.();
  }

  private untrackClaim(id: string): void {
    this.claims.delete(id);
    if (this.claims.size !== 0 || !this.renewalTimer) return;
    clearInterval(this.renewalTimer);
    this.renewalTimer = null;
  }

  private clearEphemeralForId(id: string): void {
    this.inflightQueuedIds.delete(id);
    this.nativeQueueOwnership.delete(id);
    this.pendingSendAt.delete(id);
    this.untrackClaim(id);
  }

  private clearEphemeralAuthority(): void {
    for (const id of new Set([
      ...this.claims.keys(),
      ...this.inflightQueuedIds,
      ...this.nativeQueueOwnership.keys(),
      ...this.pendingSendAt.keys(),
    ])) {
      this.clearEphemeralForId(id);
    }
    if (this.renewalTimer) {
      clearInterval(this.renewalTimer);
      this.renewalTimer = null;
    }
    for (const timer of this.leaseWakeTimers.values()) clearTimeout(timer);
    this.leaseWakeTimers.clear();
    this.emitQueueDepth();
  }

  private async renewClaims(): Promise<void> {
    await this.requestAuthority("claim-renewal");
    for (const [id, claim] of [...this.claims]) {
      const identity = this.durableIdentities.get(id);
      if (!identity) {
        this.untrackClaim(id);
        continue;
      }
      const renewed = committedOrThrow(
        "renew-claim",
        await this.durableStore.renew(identity, claim),
      );
      if (renewed.kind === "fenced") {
        this.clearEphemeralAuthority();
        this.owner = null;
        this.connectionGenerationBound = false;
        const changed = new OutboundAuthorityChangedError(
          claim.ownerId,
          claim.ownerInstanceId,
          claim.ownerGeneration,
          claim.authorityEpoch,
        );
        this.deps.onAuthorityLost(changed);
        await this.reactivateOwner();
        throw changed;
      }
      if (renewed.kind === "missing") {
        this.untrackClaim(id);
        this.inflightQueuedIds.delete(id);
        this.nativeQueueOwnership.delete(id);
        continue;
      }
      this.claims.set(id, renewed.claim);
      const ownership = this.nativeQueueOwnership.get(id);
      if (ownership) {
        this.nativeQueueOwnership.set(id, {
          ...ownership,
          claim: renewed.claim,
        });
      }
    }
  }

  /**
   * Enter terminal queue state synchronously. Browser client disposal calls
   * this before detaching its socket so no observer, renewal, owner claim, or
   * owner installation can race the exact-fence teardown reconciliation.
   */
  beginFinalDisposal(): void {
    if (this.disposed) return;
    this.disposalOwner = this.owner ? { ...this.owner } : null;
    this.disposalConnectionGeneration = this.connectionGenerationBound
      ? this.connectionGeneration
      : null;
    this.disposed = true;
    this.authorityDirty = false;
    this.authorityReasons.clear();
    if (this.authorityTimer) clearInterval(this.authorityTimer);
    this.authorityTimer = null;
    this.terminalRetryWake?.();
    this.terminalRetryWake = null;
    if (this.terminalRetryTimer) clearTimeout(this.terminalRetryTimer);
    this.terminalRetryTimer = null;
    this.clearEphemeralAuthority();
    if (this.revisionChannel) {
      this.revisionChannel.onmessage = null;
      this.revisionChannel.close();
      this.revisionChannel = null;
    }
    if (
      typeof window !== "undefined"
      && typeof window.removeEventListener === "function"
    ) {
      if (this.onlineListener) window.removeEventListener("online", this.onlineListener);
      if (this.pageShowListener) window.removeEventListener("pageshow", this.pageShowListener);
      if (
        this.visibilityListener
        && typeof document !== "undefined"
        && typeof document.removeEventListener === "function"
      ) {
        document.removeEventListener("visibilitychange", this.visibilityListener);
      }
    }
    this.onlineListener = null;
    this.pageShowListener = null;
    this.visibilityListener = null;
    this.owner = null;
    this.connectionGenerationBound = false;
  }

  dispose(): Promise<void> {
    if (this.disposePromise) return this.disposePromise;
    this.beginFinalDisposal();
    const executor = this.disposalOwner;
    const terminalDrain = executor
      ? this.serializeTerminal(() => (
          this.drainTerminalIntents(executor, true)
        ))
      : Promise.resolve();
    const operation = (async () => {
      const failures = new Set<unknown>();
      const drainResult = await Promise.allSettled([terminalDrain]);
      collectUnexpectedQueueFailures(failures, drainResult);
      try {
        await this.whenQuiescent();
      } catch (error) {
        collectQueueFailure(failures, error, isExactQueueDisposedError);
      } finally {
        this.disposalOwner = null;
        this.disposalConnectionGeneration = null;
      }
      if (failures.size > 0) {
        throw new AggregateError(
          [...failures],
          "Outbound queue disposal failed",
        );
      }
    })();
    this.disposePromise = operation;
    return this.disposePromise;
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
