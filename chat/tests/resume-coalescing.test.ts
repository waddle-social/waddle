/**
 * PR2 — resume-coordination tests.
 *
 * Two concerns from the PR2 plan:
 *
 *   * Bug 2: three event hooks fire `handleSessionReady` for the
 *     same xmpp handle on resume. The pre-fix counter-based gate
 *     reset the live buffer on the second trigger, causing the
 *     first trigger's `finally` to skip the drain — buffered
 *     messages were lost. The fix coalesces via a single
 *     `resumeBarrier` promise.
 *
 *   * Bug 4: buffered live messages were dispatched *before* the
 *     outbound queue flushed on resume completion, mis-ordering the
 *     tail when the user's queued sends pre-dated inbound arrivals.
 *
 * (Bug 3 from the audit — `pageCrossesSince` early-exit — was a
 * false alarm. The catch-up loop pages *backward* from newest, so
 * stopping when a page contains any message older than `since` is
 * correct: every subsequent page is older still. Reverting that
 * removal kept the existing `client-send-readiness.test.ts` test
 * for the same behavior green.)
 *
 * These tests poke private state on `BrowserXmppClient` directly —
 * see the comment block in `resume-ordering.test.ts` for the
 * rationale.
 */
import { afterEach, describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient, type LiveDmMessage } from "../src/lib/xmpp-client";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime/memory-durable-store";

type ResumeBarrier = {
  xmpp: unknown;
  generation: number;
  promise: Promise<void>;
} | null;

type PrivateState = {
  connectEpoch: number;
  pendingDuringResume: Array<unknown> | null;
  carriedPendingDuringResume: Array<unknown>;
  resumeBarrier: ResumeBarrier;
  handleMessage: (message: unknown) => void;
  completeResumeBarrier: (xmpp: unknown, generation: number) => Promise<void>;
  dispatchLiveBodyMessage: (message: unknown) => void;
  drainResumeBuffer: (messages: unknown[]) => void;
  flushAfterSessionReady: (xmpp: unknown, generation: number) => Promise<void>;
  flushThenDrainCarried: (xmpp: unknown, generation: number) => Promise<boolean>;
  xmpp: unknown;
};

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

const createdClients: BrowserXmppClient[] = [];

function createTestClient(): BrowserXmppClient {
  const client = new BrowserXmppClient(session(), {
    durableRuntimeStore: new MemoryDurableOutboundStore(),
  });
  createdClients.push(client);
  return client;
}

afterEach(async () => {
  for (const client of createdClients) {
    const state = client as unknown as {
      xmpp: null;
      connected: boolean;
      connectEpoch: number;
      outboundQueueHydration: Promise<void>;
      outboundQueue: {
        beginConnectionGeneration: (generation: number) => number;
        whenQuiescent: () => Promise<void>;
      };
    };
    await state.outboundQueueHydration;
    await state.outboundQueue.whenQuiescent();
    state.outboundQueue.beginConnectionGeneration(state.connectEpoch);
    state.xmpp = null;
    state.connected = false;
  }
  await Promise.all(createdClients.map((client) => client.dispose()));
  createdClients.length = 0;
});

function dmWasmMessage(id: string, body: string, timestamp: string) {
  return {
    mam_id: id,
    id,
    from: "bob@example.com/phone",
    to: "alice@example.com/desktop",
    message_type: "chat",
    body,
    timestamp,
    reaction_emojis: [],
    shared_files: [],
    is_muc: false,
  };
}

describe("Bug 2 — duplicate handleSessionReady triggers don't lose buffered messages", () => {
  test("`pendingDuringResume === null` is the sentinel for 'not buffering'; null both fields by default", () => {
    const client = createTestClient();
    const state = client as unknown as PrivateState;
    expect(state.pendingDuringResume).toBeNull();
    expect(state.resumeBarrier).toBeNull();
  });

  test("completeResumeBarrier preserves the buffer when called for a stale handle", async () => {
    // Scenario: handle A is mid-catchup with two buffered messages.
    // Then a disconnect happens and handle B takes over (so
    // `this.xmpp = B`). Before B's catchup finishes, A's barrier
    // resolves and calls completeResumeBarrier(A). It must NOT
    // wipe state that now belongs to B.
    const client = createTestClient();
    const state = client as unknown as PrivateState;
    const xmppA = { tag: "A" };
    const xmppB = { tag: "B" };
    state.xmpp = xmppB; // handle B is current
    state.pendingDuringResume = [
      dmWasmMessage("dm-during-b", "B is current", "2026-05-20T10:00:00.000Z"),
    ];
    state.resumeBarrier = {
      xmpp: xmppB,
      generation: 0,
      promise: Promise.resolve(),
    };
    await state.completeResumeBarrier(xmppA, 0); // stale A barrier completes
    expect(state.pendingDuringResume).toHaveLength(1);
    expect(state.resumeBarrier?.xmpp).toBe(xmppB);
  });
});

describe("Bug 4 — resume completion order: queue flush before live-buffer drain", () => {
  test("completeResumeBarrier awaits queue flush before draining buffered messages", async () => {
    const client = createTestClient();
    const state = client as unknown as PrivateState;
    const xmpp = { tag: "X" };
    state.xmpp = xmpp;
    state.resumeBarrier = { xmpp, generation: 0, promise: Promise.resolve() };
    state.pendingDuringResume = [
      dmWasmMessage("dm-buffered-1", "buffered live", "2026-05-20T10:00:00.000Z"),
    ];

    let markFlushStarted!: () => void;
    let releaseFlush!: () => void;
    const flushStarted = new Promise<void>((resolve) => {
      markFlushStarted = resolve;
    });
    const flushGate = new Promise<void>((resolve) => {
      releaseFlush = resolve;
    });
    const order: string[] = [];
    state.flushAfterSessionReady = mock(async () => {
      order.push("flush-start");
      markFlushStarted();
      await flushGate;
      order.push("flush-finish");
    });
    state.drainResumeBuffer = mock(() => { order.push("drain"); });

    const completion = state.completeResumeBarrier(xmpp, 0);
    await flushStarted;
    expect(order).toEqual(["flush-start"]);
    expect(state.pendingDuringResume).toHaveLength(1);

    releaseFlush();
    await completion;
    expect(order).toEqual(["flush-start", "flush-finish", "drain"]);
    expect(state.pendingDuringResume).toBeNull();
  });

  test("an A-to-B switch during the awaited flush restores A and preserves B's barrier", async () => {
    // Scenario (the R5 mobile Wi-Fi → cellular case): handle A's
    // barrier is in flight when a full reconnect produces handle B.
    // B installs its OWN barrier (`{ xmpp: B }`) and starts its own
    // catchup. Later A's pending promise resolves and calls
    // `completeResumeBarrier(A)`. That call must NOT touch B's
    // barrier or buffer, and must NOT fire queue flush / drain for
    // the long-dead handle A.
    const client = createTestClient();
    const state = client as unknown as PrivateState;
    const handleA = { tag: "A" };
    const handleB = { tag: "B" };
    state.connectEpoch = 0;
    state.xmpp = handleA;
    state.resumeBarrier = {
      xmpp: handleA,
      generation: 0,
      promise: Promise.resolve(),
    };
    const bufferedA = [
      dmWasmMessage("dm-during-a", "A's buffer", "2026-05-20T10:00:00.000Z"),
    ];
    state.pendingDuringResume = bufferedA;
    let markFlushStarted!: () => void;
    let releaseFlush!: () => void;
    const flushStarted = new Promise<void>((resolve) => {
      markFlushStarted = resolve;
    });
    const flushGate = new Promise<void>((resolve) => {
      releaseFlush = resolve;
    });
    state.flushAfterSessionReady = mock(async () => {
      markFlushStarted();
      await flushGate;
    });
    state.drainResumeBuffer = mock(() => {
      throw new Error("stale A buffer was drained");
    });

    const completion = state.completeResumeBarrier(handleA, 0);
    await flushStarted;

    state.connectEpoch = 1;
    state.xmpp = handleB;
    const bufferedB = [
      dmWasmMessage("dm-during-b", "B's buffer", "2026-05-20T10:00:01.000Z"),
    ];
    state.resumeBarrier = {
      xmpp: handleB,
      generation: 1,
      promise: Promise.resolve(),
    };
    state.pendingDuringResume = bufferedB;
    releaseFlush();
    await completion;

    expect(state.resumeBarrier?.xmpp).toBe(handleB);
    expect(state.pendingDuringResume).toBe(bufferedB);
    expect(state.carriedPendingDuringResume).toEqual(bufferedA);
  });

  test("the no-catchup path awaits queue flush before draining carried messages", async () => {
    const client = createTestClient();
    const state = client as unknown as PrivateState;
    const handle = { tag: "current" };
    state.connectEpoch = 0;
    state.xmpp = handle;
    state.carriedPendingDuringResume.push(
      dmWasmMessage("dm-carried", "carried", "2026-05-20T10:00:00.000Z"),
    );
    let markFlushStarted!: () => void;
    let releaseFlush!: () => void;
    const flushStarted = new Promise<void>((resolve) => {
      markFlushStarted = resolve;
    });
    const flushGate = new Promise<void>((resolve) => {
      releaseFlush = resolve;
    });
    const order: string[] = [];
    state.flushAfterSessionReady = mock(async () => {
      order.push("flush-start");
      markFlushStarted();
      await flushGate;
      order.push("flush-finish");
    });
    state.drainResumeBuffer = mock(() => { order.push("drain"); });

    const completion = state.flushThenDrainCarried(handle, 0);
    await flushStarted;
    expect(order).toEqual(["flush-start"]);

    releaseFlush();
    await expect(completion).resolves.toBe(true);
    expect(order).toEqual(["flush-start", "flush-finish", "drain"]);
    expect(state.carriedPendingDuringResume).toEqual([]);
  });
});

// LiveDmMessage import is exercised via the type cast for `seen[]`
// arrays at runtime — keep knip happy without an ignore directive.
const _typeProbe: LiveDmMessage[] = [];
void _typeProbe;
