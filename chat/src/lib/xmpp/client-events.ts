/**
 * Typed event bus backing `BrowserXmppClient`'s handler-registration API.
 *
 * The client historically kept ~25 nullable single-consumer handler fields
 * (`setXHandler(fn)` replaces the previous handler) plus telemetry hook
 * arrays (`onX(fn)` appends). This bus preserves both semantics:
 *
 * - `set(event, fn)`  — single-listener: replaces every listener for the
 *   event (mirrors `this.xHandler = fn`; `null` clears, mirroring
 *   `setMdsDisplayedHandler(null)` / `setPubsubEventHandler(null)`).
 * - `on(event, fn)`   — multi-listener: appends and returns an unsubscribe
 *   (mirrors `this.xHooks.push(fn)` / `pubsubEventHandlers.add`).
 * - `emit(event, …)`  — invokes listeners without error isolation, exactly
 *   like the old `this.xHandler?.(…)` calls and the pubsub fan-out loop:
 *   a throwing listener propagates to the emitter.
 * - `emitSafe(event, …)` — per-listener try/catch, mirroring the old
 *   `fireHook` used for observe-only telemetry hooks: one throwing hook
 *   must not break the others or the client.
 */

import type {
  ChatStateEvent,
  DmChatStateEvent,
  DmDisplayedEvent,
  DmReactionEvent,
  LiveDmMessage,
  LiveRoomMessage,
  PresenceUpdateEvent,
  ReactionEvent,
  RoomActivityEvent,
  RoomAuthority,
  RoomHats,
  RoomPresence,
  SessionLifecycleEvent,
  XmppErrorEvent,
  XmppStatusSnapshot,
} from "./types";
import type { InboxEntry } from "./inbox-types";
import type { TerminalMucJoinCondition } from "./room-auto-join-policy";
import type { WasmPinEvent, WasmPubsubEvent } from "./wasm-types";

/** Event name → payload tuple. */
export type ClientEventMap = Record<string, ReadonlyArray<unknown>>;

/**
 * Typed XEP-0490 entry surfaced from the WASM client into the chat
 * layer. External consumers can use TypeScript structural typing
 * (`{ chatId, stanzaId, stanzaIdBy }`) on the `setMdsDisplayedHandler`
 * callback parameter without importing this name.
 */
export interface MdsDisplayedEntry {
  /** PEP item id: bare for DMs/rooms, full occupant for MUC PMs. */
  chatId: string;
  /** XEP-0359 id of the latest displayed message. */
  stanzaId: string;
  /** JID that injected the stanza-id (room for MUC, user's server for DM). */
  stanzaIdBy: string;
}

export type PubsubEvent = WasmPubsubEvent;

/** Closed XEP-0198 observability projection from the Rust runtime. */
export type StreamManagementTelemetry =
  | { kind: "ack-requested"; reason: "outbound-stanza" | "resumed-unacked-tail" | "peer-request" | "pagehide" }
  | { kind: "ack-validated"; progress: boolean }
  | { kind: "ack-retry"; attempt: number }
  | { kind: "ack-request-timed-out" }
  | { kind: "progress-timed-out" }
  | { kind: "failed" };

type CatchupOutcome = "completed" | "aborted" | "failed";

/**
 * Reconnect catch-up failed for one conversation (#1180): the fresh
 * lifecycle event reported this conversation as covered, so its
 * consumer skipped the wholesale MAM reload — this event is the
 * fallback signal to run that reload after the failed attempt
 * (serialized, so the two fetches can no longer race).
 */
export type CatchupConversationFailure = { kind: "dm" | "room"; key: string; dmScope?: "account" | "muc-occupant" };

export type CatchupHookInfo = {
  conversations: number;
  processedConversations: number;
  pages: number;
  pageFailures: number;
  messages: number;
  durationMs: number;
  outcome: CatchupOutcome;
};

export type RoomAccessChangedEvent =
  | {
      roomJid: string;
      state: "required";
      condition: TerminalMucJoinCondition;
    }
  | {
      roomJid: string;
      state: "available";
    };

/**
 * Event map for `BrowserXmppClient`'s internal bus.
 *
 * Events registered via `set*Handler` methods use single-listener
 * (`events.set`) semantics — re-registration replaces the previous
 * handler, which `src/channels/messages.ts` relies on across room
 * switches. Events registered via `on*` hook methods and
 * `addPubsubEventHandler` use multi-listener (`events.on`) semantics.
 */
export type ClientEvents = {
  message: [message: LiveRoomMessage];
  pinEvent: [event: { roomJid: string; event: WasmPinEvent }];
  directMessage: [message: LiveDmMessage];
  status: [status: XmppStatusSnapshot];
  reaction: [event: ReactionEvent];
  displayed: [event: { roomJid: string; nick: string; messageId: string }];
  mdsDisplayed: [entry: MdsDisplayedEntry];
  pubsubEvent: [event: PubsubEvent];
  chatState: [event: ChatStateEvent];
  dmChatState: [event: DmChatStateEvent];
  dmReaction: [event: DmReactionEvent];
  dmDisplayed: [event: DmDisplayedEvent];
  presenceUpdate: [event: PresenceUpdateEvent];
  memberJid: [nick: string, bareJid: string];
  hats: [hats: RoomHats];
  authority: [authority: RoomAuthority];
  activity: [event: RoomActivityEvent];
  inboxPush: [entry: InboxEntry];
  roomAvatar: [roomJid: string, hash: string];
  roomDisconnect: [];
  presence: [presence: RoomPresence];
  lastSeen: [nick: string, timestamp: number];
  messageAck: [messageId: string];
  messageDeliveryFailure: [messageId: string];
  queuedMessageStatus: [messageId: string, status: "queued" | "sending"];
  sessionLifecycle: [event: SessionLifecycleEvent];
  catchupFailure: [failure: CatchupConversationFailure];
  roomAccessChanged: [event: RoomAccessChangedEvent];
  // Observe-only telemetry hooks (multi-listener, error-isolated via
  // `emitSafe`). Includes the background-tab RESULT_CODE_HUNG health
  // hooks (see docs/planning/hung-tab-investigation.md); the client
  // stays telemetry-agnostic — xmpp-instrumentation.ts binds these to
  // the Faro report* sink.
  messageAcked: [id: string, meta: { kind: "room" | "dm"; latencyMs: number }];
  messageDeliveryFailed: [id: string, meta: { kind: "room" | "dm" }];
  sessionLifecycleHook: [event: SessionLifecycleEvent];
  statusHook: [status: XmppStatusSnapshot, meta: { reconnectDurationMs?: number }];
  sendEnqueued: [info: { kind: "room" | "dm"; reason: string }];
  queueDepthChange: [depth: { kind: "room" | "dm"; persisted: number; inflight: number }];
  error: [event: XmppErrorEvent];
  reconnectScheduled: [info: { attempt: number; delayMs: number }];
  catchup: [info: CatchupHookInfo];
  resumeDrain: [info: { buffered: number; durationMs: number }];
  streamManagement: [event: StreamManagementTelemetry];
};

type GenericListener = (...args: ReadonlyArray<unknown>) => void;

export class TypedEventBus<Events extends ClientEventMap> {
  private readonly listeners = new Map<keyof Events, Set<GenericListener>>();

  /** Append a listener. Returns an unsubscribe function. */
  on<K extends keyof Events>(event: K, listener: (...args: Events[K]) => void): () => void {
    let set = this.listeners.get(event);
    if (!set) {
      set = new Set();
      this.listeners.set(event, set);
    }
    const generic = listener as GenericListener;
    set.add(generic);
    return () => {
      this.listeners.get(event)?.delete(generic);
    };
  }

  /**
   * Single-listener registration: replaces every listener for `event`
   * (matching the old `this.xHandler = fn` setter semantics). Passing
   * `null` clears the event's listeners.
   */
  set<K extends keyof Events>(event: K, listener: ((...args: Events[K]) => void) | null): void {
    if (!listener) {
      this.listeners.delete(event);
      return;
    }
    this.listeners.set(event, new Set([listener as GenericListener]));
  }

  /**
   * Invoke listeners without error isolation — a throwing listener
   * propagates, exactly like the old direct `this.xHandler?.(…)` calls.
   */
  emit<K extends keyof Events>(event: K, ...args: Events[K]): void {
    const set = this.listeners.get(event);
    if (!set || set.size === 0) return;
    for (const listener of [...set]) {
      listener(...args);
    }
  }

  /**
   * Invoke listeners with per-listener error isolation — mirrors the old
   * `fireHook` behavior for observe-only telemetry hooks.
   */
  emitSafe<K extends keyof Events>(event: K, ...args: Events[K]): void {
    const set = this.listeners.get(event);
    if (!set || set.size === 0) return;
    for (const listener of [...set]) {
      try {
        listener(...args);
      } catch (error) {
        console.error("xmpp telemetry hook threw", error);
      }
    }
  }
}
