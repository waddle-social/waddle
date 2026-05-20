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
import { describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient, type LiveDmMessage } from "../src/lib/xmpp-client";

type ResumeBarrier = { xmpp: unknown; promise: Promise<void> } | null;

type PrivateState = {
  pendingDuringResume: Array<unknown> | null;
  resumeBarrier: ResumeBarrier;
  handleMessage: (message: unknown) => void;
  completeResumeBarrier: (xmpp: unknown) => void;
  dispatchLiveBodyMessage: (message: unknown) => void;
  flushQueuedDirectMessages: () => Promise<void>;
  flushQueuedRoomMessages: (roomJid: string) => Promise<void>;
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
    const client = new BrowserXmppClient(session());
    const state = client as unknown as PrivateState;
    expect(state.pendingDuringResume).toBeNull();
    expect(state.resumeBarrier).toBeNull();
  });

  test("completeResumeBarrier preserves the buffer when called for a stale handle", () => {
    // Scenario: handle A is mid-catchup with two buffered messages.
    // Then a disconnect happens and handle B takes over (so
    // `this.xmpp = B`). Before B's catchup finishes, A's barrier
    // resolves and calls completeResumeBarrier(A). It must NOT
    // wipe state that now belongs to B.
    const client = new BrowserXmppClient(session());
    const state = client as unknown as PrivateState;
    const xmppA = { tag: "A" };
    const xmppB = { tag: "B" };
    state.xmpp = xmppB; // handle B is current
    state.pendingDuringResume = [
      dmWasmMessage("dm-during-b", "B is current", "2026-05-20T10:00:00.000Z"),
    ];
    state.resumeBarrier = { xmpp: xmppB, promise: Promise.resolve() };
    state.completeResumeBarrier(xmppA); // stale A barrier completes
    expect(state.pendingDuringResume).toHaveLength(1);
    expect(state.resumeBarrier?.xmpp).toBe(xmppB);
  });
});

describe("Bug 4 — resume completion order: queue flush before live-buffer drain", () => {
  test("completeResumeBarrier flushes queue before dispatching buffered messages", () => {
    const client = new BrowserXmppClient(session());
    const state = client as unknown as PrivateState;
    const xmpp = { tag: "X" };
    state.xmpp = xmpp;
    state.resumeBarrier = { xmpp, promise: Promise.resolve() };
    state.pendingDuringResume = [
      dmWasmMessage("dm-buffered-1", "buffered live", "2026-05-20T10:00:00.000Z"),
    ];

    // Spy on the two side-effects in order.
    const order: string[] = [];
    state.flushQueuedDirectMessages = mock(async () => { order.push("flushDM"); });
    state.flushQueuedRoomMessages = mock(async () => { order.push("flushRoom"); });
    state.dispatchLiveBodyMessage = mock(() => { order.push("drain"); });

    state.completeResumeBarrier(xmpp);

    // Queue flush is fire-and-forget (void), but the synchronous
    // calls to flushQueuedDirectMessages / flushQueuedRoomMessages
    // happen before the buffer-drain loop. We assert the first
    // recorded side-effect is "flushDM" and the last is "drain".
    expect(order[0]).toBe("flushDM");
    expect(order[order.length - 1]).toBe("drain");
  });

  test("a stale handle's completion does NOT clear the current handle's barrier (R5)", () => {
    // Scenario (the R5 mobile Wi-Fi → cellular case): handle A's
    // barrier is in flight when a full reconnect produces handle B.
    // B installs its OWN barrier (`{ xmpp: B }`) and starts its own
    // catchup. Later A's pending promise resolves and calls
    // `completeResumeBarrier(A)`. That call must NOT touch B's
    // barrier or buffer, and must NOT fire queue flush / drain for
    // the long-dead handle A.
    const client = new BrowserXmppClient(session());
    const state = client as unknown as PrivateState;
    const handleA = { tag: "A" };
    const handleB = { tag: "B" };
    state.xmpp = handleB;
    state.resumeBarrier = { xmpp: handleB, promise: Promise.resolve() };
    state.pendingDuringResume = [
      dmWasmMessage("dm-during-b", "B's buffer", "2026-05-20T10:00:00.000Z"),
    ];

    let flushed = false, drained = false;
    state.flushQueuedDirectMessages = mock(async () => { flushed = true; });
    state.dispatchLiveBodyMessage = mock(() => { drained = true; });

    state.completeResumeBarrier(handleA); // stale A barrier resolves

    expect(flushed).toBe(false);
    expect(drained).toBe(false);
    // B's barrier is untouched; B's buffer is untouched.
    expect(state.resumeBarrier?.xmpp).toBe(handleB);
    expect(state.pendingDuringResume).toHaveLength(1);
  });
});

// LiveDmMessage import is exercised via the type cast for `seen[]`
// arrays at runtime — keep knip happy without an ignore directive.
const _typeProbe: LiveDmMessage[] = [];
void _typeProbe;
