import { afterAll, afterEach, beforeAll, describe, expect, mock, test } from "bun:test";
import {
  $callState,
  $lastCallError,
  beginOutgoingCall,
  clearCallState,
  configureIncomingCallAlerts,
  type RawIqSender,
  scheduleOutgoingTimeout,
  setSessionAcceptTimeoutMsForTests,
  tearDownActiveCall,
} from "../src/lib/calls/call-store";
import {
  $mucCallParticipantOwners,
  $mucCallParticipants,
  applyMucCallPresence,
  clearMucCallParticipants,
} from "../src/lib/calls/muc-call-presence";
import { applyDmCallEvent, clearDmCallActivities, readDmCallActivity } from "../src/lib/calls/dm-call-activity";
import { $dmCallOutcomeAnchor } from "../src/lib/calls/dm-call-anchor";
import { leaveRetainedMucCallAction, startMucCallAction } from "../src/lib/calls/muc-call-actions";
import {
  $mucCallTerminatePendingSessions,
  clearAllMucCallSessionCacheForTests,
  markMucCallSessionTerminatePending,
  readMucCallSession,
  rememberMucCallSession,
} from "../src/lib/calls/muc-call-session-cache";
import type { CallWireSender } from "../src/lib/calls/outbound";
import type { CallEvent, CallMedia, LiveKitJoin } from "../src/lib/calls/types";
import { createIncomingCallAlertController } from "../src/shell/audio-alerts";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import { __resetCallLifecycleTelemetryForTesting } from "../src/lib/calls/call-lifecycle-telemetry";
import { __setFaroForTesting } from "../src/lib/telemetry";

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    username: "alice",
    jid: "alice@waddle.test/web",
    session_id: "tok",
    xmpp_websocket_url: "wss://waddle.test/ws",
    ...partial,
  } as WaddleSession;
}

function wireClientEvents(sender: Partial<CallWireSender> = {}) {
  let onPresence: ((presence: {
    from?: string;
    presence_type?: string;
    muc_jid?: string | null;
    muji?: { preparing: boolean; active: boolean };
  }) => void) | null = null;
  let onCall: ((event: CallEvent) => void) | null = null;
  let onDisconnected: (() => void) | null = null;
  const client = new BrowserXmppClient(session());
  const xmpp = {
    set_on_presence: (cb: NonNullable<typeof onPresence>) => {
      onPresence = cb;
    },
    set_on_call: (cb: NonNullable<typeof onCall>) => {
      onCall = cb;
    },
    set_on_disconnected: (cb: NonNullable<typeof onDisconnected>) => {
      onDisconnected = cb;
    },
    ...sender,
  };
  (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);
  return {
    client,
    xmpp,
    emitPresence(presence: Parameters<NonNullable<typeof onPresence>>[0]) {
      onPresence?.(presence);
    },
    emitCall(event: CallEvent) {
      onCall?.(event);
    },
    emitDisconnected() {
      onDisconnected?.();
    },
  };
}

async function flushCallSideEffects(): Promise<void> {
  for (let i = 0; i < 8; i += 1) {
    await Promise.resolve();
  }
}

function firstMockCallArg(fn: unknown, index: number): unknown {
  return (fn as { mock: { calls: unknown[][] } }).mock.calls[0]?.[index];
}

function wireEmitterPresenceEvents() {
  let onPresence: ((presence: {
    from?: string;
    presence_type?: string;
    muji?: { preparing: boolean; active: boolean };
  }) => void) | null = null;
  const client = new BrowserXmppClient(session());
  const xmpp = {
    on: (event: string, cb: typeof onPresence) => {
      if (event === "presence") onPresence = cb;
    },
  };
  (client as unknown as { wireEvents: (xmpp: typeof xmpp) => void }).wireEvents(xmpp);
  return {
    emitPresence(presence: Parameters<NonNullable<typeof onPresence>>[0]) {
      onPresence?.(presence);
    },
  };
}

/**
 * Mock the wasm `update_muji_presence` such that the preparing
 * branch simulates the MUC echoing the preparing presence back
 * — the XEP-0272 §Joining echo that `awaitPreparingEcho` blocks
 * on inside `beginMucCall`. Without this the tests would all
 * pay the 2s echo-timeout penalty on every call to `beginMucCall`
 * that passes a `selfNick`.
 */
function mockUpdateMujiPresenceWithEcho(
  emitPresence: ReturnType<typeof wireClientEvents>["emitPresence"],
  mucJid?: string | null,
) {
  return mock(
    async (
      roomJid: string,
      nick: string,
      active: boolean,
      preparing: boolean,
      _video: boolean,
    ) => {
      if (preparing) {
        emitPresence({
          from: `${roomJid}/${nick}`,
          presence_type: "available",
          muc_jid: mucJid,
          muji: { preparing: true, active: false },
        });
      }
      if (active) {
        emitPresence({
          from: `${roomJid}/${nick}`,
          presence_type: "available",
          muc_jid: mucJid,
          muji: { preparing: false, active: true },
        });
      }
    },
  );
}

/**
 * Mock the wasm `send_muji_session_initiate` for the XEP-0166
 * §6.3 separate-IQ accept flow. After resolving the empty IQ-result
 * ack, fires `applyCallEvent` with the typed `session-accept` event
 * the chat-side's `tryFulfillMujiAccept` is waiting on. The real
 * wasm bridge would parse this from an inbound `<iq type='set'>`
 * stanza routed through `on_call`; tests skip the wire dance.
 */
function mockSendMujiSessionInitiateWithAccept(
  join: LiveKitJoin,
  emitCall: ReturnType<typeof wireClientEvents>["emitCall"],
) {
  return mock(async (_roomJid: string, sid: string, _video: boolean) => {
    // Empty IQ-result ack is the function's resolution; fire the
    // inbound session-accept on the next microtask so the await
    // chain in beginMucCall sees the resolver populated before the
    // event lands.
    queueMicrotask(() => {
      emitCall({
        kind: "session-accept",
        from: "calls.waddle.test",
        sid,
        media: { audio: true, video: true },
        join,
      });
    });
  });
}

const audioVideo: CallMedia = { audio: true, video: true };
const join: LiveKitJoin = {
  url: "wss://livekit.test",
  room: "alice@waddle.test::c1",
  identity: "bob@waddle.test/desktop",
  token: "eyJhbGc.payload.sig",
};

function mockSender(): CallWireSender {
  return {
    send_call_propose: mock(async () => undefined),
    send_call_proceed: mock(async () => undefined),
    send_call_ringing: mock(async () => undefined),
    send_call_reject: mock(async () => undefined),
    send_call_retract: mock(async () => undefined),
    send_call_finish: mock(async () => undefined),
    send_call_finish_migrated: mock(async () => undefined),
    send_call_session_initiate: mock(async () => undefined),
    send_call_session_accept: mock(async () => undefined),
    send_call_session_terminate: mock(async () => undefined),
  };
}

const WINDOW_SENTINEL = Symbol("call-teardown-window");
type ShimmedGlobal = typeof globalThis & {
  window?: { localStorage: Storage } & { [WINDOW_SENTINEL]?: true };
};

beforeAll(() => {
  const g = globalThis as ShimmedGlobal;
  if (typeof g.window !== "undefined") return;
  const store = new Map<string, string>();
  const storage: Storage = {
    get length() { return store.size; },
    clear: () => store.clear(),
    getItem: (key) => store.get(key) ?? null,
    key: (index) => Array.from(store.keys())[index] ?? null,
    removeItem: (key) => { store.delete(key); },
    setItem: (key, value) => { store.set(key, String(value)); },
  };
  g.window = Object.assign({ localStorage: storage }, { [WINDOW_SENTINEL]: true as const });
});

afterAll(() => {
  const g = globalThis as ShimmedGlobal;
  if (g.window?.[WINDOW_SENTINEL]) {
    delete (g as { window?: unknown }).window;
  }
});

afterEach(() => {
  clearCallState();
  clearDmCallActivities();
  $dmCallOutcomeAnchor.set(null);
  configureIncomingCallAlerts(null);
  $lastCallError.set(null);
  // The Muji mocks above fire `applyMucCallPresence` to simulate
  // MUC echoes; clearing the participants store between tests
  // keeps that state from leaking into other test files (e.g.
  // `muc-call-presence.test.ts`) that expect an empty store.
  clearMucCallParticipants();
  clearAllMucCallSessionCacheForTests();
  __resetCallLifecycleTelemetryForTesting();
  __setFaroForTesting(null);
});

describe("DM call outcome feed anchors", () => {
  test("live reject records a declined entry for the peer feed", () => {
    const events = wireClientEvents();

    events.emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      to: "alice@waddle.test/web",
      sid: "c-decline",
      media: audioVideo,
    });
    events.emitCall({
      kind: "reject",
      from: "alice@waddle.test/web",
      to: "bob@waddle.test/phone",
      sid: "c-decline",
    });

    expect($dmCallOutcomeAnchor.get()).toMatchObject({
      peerBareJid: "bob@waddle.test",
      sid: "c-decline",
      outcome: "declined",
    });
  });

  test("caller retract records missed for the recipient feed", () => {
    const events = wireClientEvents();

    events.emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      to: "alice@waddle.test/web",
      sid: "c-missed",
      media: audioVideo,
    });
    events.emitCall({
      kind: "retract",
      from: "bob@waddle.test/phone",
      to: "alice@waddle.test/web",
      sid: "c-missed",
    });

    expect($dmCallOutcomeAnchor.get()).toMatchObject({
      peerBareJid: "bob@waddle.test",
      sid: "c-missed",
      outcome: "missed",
    });
  });

  test("outgoing unanswered timeout records no-answer for the caller feed", async () => {
    const sender = mockSender();

    beginOutgoingCall("bob@waddle.test", "c-no-answer", audioVideo, "alice@waddle.test/web");
    scheduleOutgoingTimeout(sender, "c-no-answer", 10);

    await new Promise((resolve) => setTimeout(resolve, 50));

    expect($dmCallOutcomeAnchor.get()).toMatchObject({
      peerBareJid: "bob@waddle.test",
      sid: "c-no-answer",
      outcome: "no-answer",
    });
  });

  test("completed call terminate records an ended entry for the feed", () => {
    const events = wireClientEvents();

    beginOutgoingCall("bob@waddle.test", "c-ended", audioVideo, "alice@waddle.test/web");
    events.emitCall({
      kind: "session-accept",
      from: "bob@waddle.test/phone",
      to: "alice@waddle.test/web",
      sid: "c-ended",
      media: audioVideo,
      join,
    });
    events.emitCall({
      kind: "session-terminate",
      from: "bob@waddle.test/phone",
      to: "alice@waddle.test/web",
      sid: "c-ended",
      reason: "success",
    });

    expect($dmCallOutcomeAnchor.get()).toMatchObject({
      peerBareJid: "bob@waddle.test",
      sid: "c-ended",
      outcome: "ended",
    });
  });

  test("timeout session-terminate records no-answer rather than ended", () => {
    applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: "2026-06-09T12:00:00Z",
      now: new Date("2026-06-09T12:01:00Z"),
      event: {
        kind: "session-terminate",
        from: "alice@waddle.test/web",
        to: "bob@waddle.test/phone",
        sid: "c-timeout-terminate",
        reason: "timeout",
      },
    });

    expect($dmCallOutcomeAnchor.get()).toMatchObject({
      peerBareJid: "bob@waddle.test",
      sid: "c-timeout-terminate",
      outcome: "no-answer",
    });
  });

  test("later finish after timeout terminate does not create a second outcome", () => {
    const timeoutOutcome = applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: "2026-06-09T12:00:45Z",
      now: new Date("2026-06-09T12:01:00Z"),
      event: {
        kind: "session-terminate",
        from: "alice@waddle.test/web",
        to: "bob@waddle.test/phone",
        sid: "c-timeout-finish",
        reason: "timeout",
      },
    });
    const firstPublished = $dmCallOutcomeAnchor.get();

    const finishOutcome = applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: "2026-06-09T12:00:46Z",
      now: new Date("2026-06-09T12:01:00Z"),
      event: {
        kind: "finish",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "c-timeout-finish",
      },
    });

    expect(timeoutOutcome?.outcome).toBe("no-answer");
    expect(finishOutcome).toBeNull();
    expect($dmCallOutcomeAnchor.get()).toEqual(firstPublished);
  });

  test("archived MAM call events derive a missed entry after reconnect", () => {
    applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: "2026-06-09T12:00:00Z",
      now: new Date("2026-06-09T12:01:00Z"),
      event: {
        kind: "propose",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "c-offline",
        media: audioVideo,
      },
    });
    applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: "2026-06-09T12:00:45Z",
      now: new Date("2026-06-09T12:01:00Z"),
      event: {
        kind: "retract",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "c-offline",
      },
    });

    expect($dmCallOutcomeAnchor.get()).toMatchObject({
      peerBareJid: "bob@waddle.test",
      sid: "c-offline",
      outcome: "missed",
      ended: "2026-06-09T12:00:45Z",
    });
  });

  test("duplicate terminal replay does not republish with fallback media", () => {
    applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: "2026-06-09T12:00:00Z",
      now: new Date("2026-06-09T12:01:00Z"),
      event: {
        kind: "propose",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "c-duplicate",
        media: audioVideo,
      },
    });
    applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: "2026-06-09T12:00:45Z",
      now: new Date("2026-06-09T12:01:00Z"),
      event: {
        kind: "retract",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "c-duplicate",
      },
    });
    const firstOutcome = $dmCallOutcomeAnchor.get();

    const duplicateOutcome = applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: "2026-06-09T12:00:45Z",
      now: new Date("2026-06-09T12:01:00Z"),
      event: {
        kind: "retract",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "c-duplicate",
      },
    });

    expect(duplicateOutcome).toBeNull();
    expect($dmCallOutcomeAnchor.get()).toEqual(firstOutcome);
    expect($dmCallOutcomeAnchor.get()?.media).toEqual(audioVideo);
  });

  test("background MAM call hydration can update activity without publishing feed outcomes", () => {
    applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: "2026-06-09T12:00:00Z",
      now: new Date("2026-06-09T12:01:00Z"),
      publishOutcome: false,
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        to: "alice@waddle.test/web",
        sid: "c-silent-hydrate",
        media: audioVideo,
        join,
      },
    });

    expect($dmCallOutcomeAnchor.get()).toBeNull();
    expect(readDmCallActivity("bob@waddle.test", new Date("2026-06-09T12:01:00Z"))).toMatchObject({
      peerJid: "bob@waddle.test",
      sid: "c-silent-hydrate",
      state: "accepted",
    });
  });

  test("lone archived self-reject attributes the declined call to the peer initiator", () => {
    applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: "2026-06-09T12:00:45Z",
      now: new Date("2026-06-09T12:01:00Z"),
      event: {
        kind: "reject",
        from: "alice@waddle.test/web",
        to: "bob@waddle.test/phone",
        sid: "c-lone-reject",
      },
    });

    expect($dmCallOutcomeAnchor.get()).toMatchObject({
      peerBareJid: "bob@waddle.test",
      sid: "c-lone-reject",
      outcome: "declined",
      initiator: "bob@waddle.test",
    });
  });
});

describe("incoming DM call alerting", () => {
  test("incoming propose starts ringtone, sends ringing, and local decline stops it", async () => {
    const sender = mockSender();
    const player = {
      startLoop: mock(() => undefined),
      stop: mock(() => undefined),
    };
    configureIncomingCallAlerts(createIncomingCallAlertController({
      player,
      isTabFocused: () => true,
    }));

    const { emitCall } = wireClientEvents(sender);
    emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      sid: "c1",
      media: audioVideo,
    });
    await flushCallSideEffects();

    expect(player.startLoop).toHaveBeenCalledWith("c1");
    expect(sender.send_call_ringing).toHaveBeenCalledWith("bob@waddle.test", "c1");

    await tearDownActiveCall(sender, "gone");
    expect(player.stop).toHaveBeenCalledWith("c1");
  });

  test("answering locally stops the ringtone when session-initiate arrives", () => {
    const player = {
      startLoop: mock(() => undefined),
      stop: mock(() => undefined),
    };
    configureIncomingCallAlerts(createIncomingCallAlertController({
      player,
      isTabFocused: () => true,
    }));

    const { emitCall } = wireClientEvents(mockSender());
    emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      sid: "c1",
      media: audioVideo,
    });
    emitCall({
      kind: "session-initiate",
      from: "bob@waddle.test/phone",
      sid: "c1",
      media: audioVideo,
      join,
    });

    expect(player.stop).toHaveBeenCalledWith("c1");
    expect($callState.get()).toMatchObject({ phase: "active", sid: "c1", kind: "dm" });
  });

  test("declining on a sibling resource stops local ringing from carbon events", async () => {
    const player = {
      startLoop: mock(() => undefined),
      stop: mock(() => undefined),
    };
    configureIncomingCallAlerts(createIncomingCallAlertController({
      player,
      isTabFocused: () => true,
    }));
    const { emitCall } = wireClientEvents(mockSender());
    emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      sid: "c1",
      media: audioVideo,
    });
    emitCall({
      kind: "reject",
      from: "alice@waddle.test/tablet",
      to: "bob@waddle.test/phone",
      sid: "c1",
    });

    expect(player.stop).toHaveBeenCalledWith("c1");
  });

  test("answering on a sibling resource stops local ringing from carbon events", () => {
    const player = {
      startLoop: mock(() => undefined),
      stop: mock(() => undefined),
    };
    configureIncomingCallAlerts(createIncomingCallAlertController({
      player,
      isTabFocused: () => true,
    }));
    const { emitCall } = wireClientEvents(mockSender());
    emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      sid: "c1",
      media: audioVideo,
    });
    emitCall({
      kind: "proceed",
      from: "alice@waddle.test/tablet",
      to: "bob@waddle.test/phone",
      sid: "c1",
    });

    expect(player.stop).toHaveBeenCalledWith("c1");
  });

  test("caller retract stops ringing and dismisses the incoming call slot", () => {
    const player = {
      startLoop: mock(() => undefined),
      stop: mock(() => undefined),
    };
    configureIncomingCallAlerts(createIncomingCallAlertController({
      player,
      isTabFocused: () => true,
    }));
    const { emitCall } = wireClientEvents(mockSender());
    emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      sid: "c1",
      media: audioVideo,
    });
    emitCall({
      kind: "retract",
      from: "bob@waddle.test/phone",
      sid: "c1",
    });

    expect(player.stop).toHaveBeenCalledWith("c1");
    expect($callState.get()).toEqual({ phase: "ended", sid: "c1", reason: "retract" });
  });

  test("unfocused tab shows incoming-call notification whose click focuses the DM", () => {
    const focused: string[] = [];
    let click: (() => void) | null = null;
    const notifier = {
      showIncomingCall: mock((options: { peerJid: string; onClick: () => void }) => {
        click = options.onClick;
        return { close: mock(() => undefined) };
      }),
    };
    configureIncomingCallAlerts(createIncomingCallAlertController({
      player: { startLoop: mock(() => undefined), stop: mock(() => undefined) },
      notifier,
      focusTarget: {
        focusConversation(peerJid) {
          focused.push(peerJid);
        },
      },
      isTabFocused: () => false,
    }));
    const { emitCall } = wireClientEvents(mockSender());
    emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      sid: "c1",
      media: audioVideo,
    });

    expect(notifier.showIncomingCall).toHaveBeenCalledTimes(1);
    click?.();
    expect(focused).toEqual(["bob@waddle.test"]);
  });

  test("ringing event marks the caller's outgoing call as ringing", () => {
    const { emitCall } = wireClientEvents(mockSender());
    beginOutgoingCall("bob@waddle.test", "c1", audioVideo, "alice@waddle.test/web");
    emitCall({
      kind: "ringing",
      from: "bob@waddle.test/phone",
      sid: "c1",
    });

    expect($callState.get()).toMatchObject({ phase: "outgoing", sid: "c1", ringing: true });
  });

  test("ringing event from another bare JID does not mark outgoing call as ringing", () => {
    const { emitCall } = wireClientEvents(mockSender());
    beginOutgoingCall("bob@waddle.test", "c1", audioVideo, "alice@waddle.test/web");
    emitCall({
      kind: "ringing",
      from: "mallory@waddle.test/phone",
      sid: "c1",
    });

    expect($callState.get()).toEqual({
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "c1",
      media: audioVideo,
      initiator: "alice@waddle.test/web",
    });
  });

  test("propose during an active call is rejected without audible ringing", async () => {
    const sender = mockSender();
    const player = {
      startLoop: mock(() => undefined),
      stop: mock(() => undefined),
    };
    configureIncomingCallAlerts(createIncomingCallAlertController({
      player,
      isTabFocused: () => true,
    }));
    $callState.set({
      phase: "active",
      peer: "carol@waddle.test/laptop",
      sid: "active-1",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    const { emitCall } = wireClientEvents(sender);
    emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      sid: "c2",
      media: audioVideo,
    });
    await flushCallSideEffects();

    expect(player.startLoop).not.toHaveBeenCalled();
    expect(sender.send_call_reject).toHaveBeenCalledWith("bob@waddle.test/phone", "c2");
  });
});

describe("tearDownActiveCall", () => {
  test("active call: dispatches sessionTerminate with the given reason", async () => {
    const sender = mockSender();
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    await tearDownActiveCall(sender, "gone");
    expect(sender.send_call_session_terminate).toHaveBeenCalledTimes(1);
    expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
      "bob@waddle.test/desktop",
      "c1",
      "gone",
    );
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("active DM call: also dispatches XEP-0353 finish after sessionTerminate", async () => {
    // XEP-0353 §3.5: after either side terminates a call, both SHOULD
    // emit `<finish/>` so the MAM bookend pairing stays consistent.
    // Without this the responder's archive only carries the `propose`
    // / `proceed` half of the JMI handshake and the initiator's UI
    // can't render a clean "call ended" row.
    const sender = mockSender();
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    await tearDownActiveCall(sender, "success");
    expect(sender.send_call_session_terminate).toHaveBeenCalledTimes(1);
    expect(sender.send_call_finish).toHaveBeenCalledTimes(1);
    expect(sender.send_call_finish).toHaveBeenCalledWith(
      "bob@waddle.test/desktop",
      "c1",
    );
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("active DM call: finish failure does not block clearing the slot", async () => {
    // A stale wasm bundle without `send_call_finish` (or a transient
    // I/O error inside the finish stanza) MUST NOT keep us stuck on
    // `phase: active` after the terminate already went out — the
    // local UI would still render the call overlay while the peer
    // has already ended.
    const sender: CallWireSender = {
      send_call_session_terminate: mock(async () => undefined),
      // No send_call_finish → outboundCalls.finish throws.
    };
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    await tearDownActiveCall(sender, "success");
    expect(sender.send_call_session_terminate).toHaveBeenCalledTimes(1);
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("active DM call: orphaned terminate still dispatches XEP-0353 finish", async () => {
    const sender: CallWireSender = {
      send_call_session_terminate_with_outcome: mock(async () => ({ kind: "orphaned" })),
      send_call_finish: mock(async () => undefined),
    };
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    await tearDownActiveCall(sender, "success");
    expect(sender.send_call_session_terminate_with_outcome).toHaveBeenCalledWith(
      "bob@waddle.test/desktop",
      "c1",
      "success",
    );
    expect(sender.send_call_finish).toHaveBeenCalledWith(
      "bob@waddle.test/desktop",
      "c1",
    );
    expect($callState.get()).toEqual({ phase: "idle" });
    expect($lastCallError.get()).toBeNull();
  });

  test("active DM call: unclassified terminate failure does not send finish", async () => {
    const sender: CallWireSender = {
      send_call_session_terminate: mock(async () => {
        throw new Error("terminate failed");
      }),
      send_call_finish: mock(async () => undefined),
    };
    applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: new Date(Date.now() - 1_000).toISOString(),
      event: {
        kind: "propose",
        from: "bob@waddle.test/desktop",
        to: "alice@waddle.test/web",
        sid: "c1",
        media: audioVideo,
      },
    });
    applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: new Date().toISOString(),
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/desktop",
        to: "alice@waddle.test/web",
        sid: "c1",
        media: audioVideo,
        join,
      },
    });
    expect(readDmCallActivity("bob@waddle.test")).toMatchObject({ sid: "c1" });
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    await tearDownActiveCall(sender, "success");
    expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
      "bob@waddle.test/desktop",
      "c1",
      "success",
    );
    expect(sender.send_call_finish).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
    expect($lastCallError.get()).toBe("terminate failed");
    expect(readDmCallActivity("bob@waddle.test")).toBeNull();
  });

  test("active DM call: typed terminate error does not send finish", async () => {
    const sender: CallWireSender = {
      send_call_session_terminate_with_outcome: mock(async () => ({ kind: "error" })),
      send_call_finish: mock(async () => undefined),
    };
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    await tearDownActiveCall(sender, "success");
    expect(sender.send_call_session_terminate_with_outcome).toHaveBeenCalledWith(
      "bob@waddle.test/desktop",
      "c1",
      "success",
    );
    expect(sender.send_call_finish).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
    expect($lastCallError.get()).toBe("call session terminate failed");
  });

  test("active DM call: typed terminate error still clears dock activity", async () => {
    const sender: CallWireSender = {
      send_call_session_terminate_with_outcome: mock(async () => ({ kind: "error" })),
      send_call_finish: mock(async () => undefined),
    };
    applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: new Date(Date.now() - 1_000).toISOString(),
      event: {
        kind: "propose",
        from: "bob@waddle.test/desktop",
        to: "alice@waddle.test/web",
        sid: "c1",
        media: audioVideo,
      },
    });
    applyDmCallEvent({
      selfBareJid: "alice@waddle.test",
      selfFullJid: "alice@waddle.test/web",
      timestamp: new Date().toISOString(),
      event: {
        kind: "session-accept",
        from: "bob@waddle.test/desktop",
        to: "alice@waddle.test/web",
        sid: "c1",
        media: audioVideo,
        join,
      },
    });
    expect(readDmCallActivity("bob@waddle.test")).toMatchObject({ sid: "c1" });
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });

    await tearDownActiveCall(sender, "success");

    expect(sender.send_call_finish).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
    expect(readDmCallActivity("bob@waddle.test")).toBeNull();
  });

  test("active call: success reason for graceful logout", async () => {
    const sender = mockSender();
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    await tearDownActiveCall(sender, "success");
    expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
      "bob@waddle.test/desktop",
      "c1",
      "success",
    );
  });

  test("outgoing call: dispatches retract", async () => {
    const sender = mockSender();
    beginOutgoingCall("bob@waddle.test", "c2", audioVideo);
    await tearDownActiveCall(sender, "gone");
    expect(sender.send_call_retract).toHaveBeenCalledTimes(1);
    expect(sender.send_call_retract).toHaveBeenCalledWith("bob@waddle.test", "c2");
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("incoming call: dispatches reject", async () => {
    const sender = mockSender();
    $callState.set({
      phase: "incoming",
      from: "alice@waddle.test/desktop",
      sid: "c3",
      media: audioVideo,
    });
    await tearDownActiveCall(sender, "gone");
    expect(sender.send_call_reject).toHaveBeenCalledTimes(1);
    expect(sender.send_call_reject).toHaveBeenCalledWith("alice@waddle.test/desktop", "c3");
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("idle: no-op", async () => {
    const sender = mockSender();
    await tearDownActiveCall(sender, "gone");
    expect(sender.send_call_session_terminate).not.toHaveBeenCalled();
    expect(sender.send_call_retract).not.toHaveBeenCalled();
    expect(sender.send_call_reject).not.toHaveBeenCalled();
  });

  test("null sender: still clears state without throwing", async () => {
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    await tearDownActiveCall(null, "gone");
    expect($callState.get()).toEqual({ phase: "idle" });
  });
});

describe("leaveRetainedMucCallAction", () => {
  test("clears this refreshed browser resource's Muji presence without dropping same-nick siblings", async () => {
    applyMucCallPresence({
      from: "chan@muc.test/alice",
      muc_jid: "alice@waddle.test/web",
      muji: { preparing: false, active: true },
    });
    applyMucCallPresence({
      from: "chan@muc.test/alice",
      muc_jid: "alice@waddle.test/phone",
      muji: { preparing: false, active: true },
    });
    const sender: RawIqSender = {
      update_muji_presence: mock(async () => undefined),
    };

    await leaveRetainedMucCallAction({
      roomJid: "chan@muc.test",
      getSender: () => sender,
      getSelfNick: () => "alice",
      getSelfFullJid: () => "alice@waddle.test/web",
    });

    expect(sender.update_muji_presence).toHaveBeenCalledWith(
      "chan@muc.test",
      "alice",
      false,
      false,
      false,
      { handRaised: false, muted: false }, // leaving clears all in-call state (#1030)
    );
    expect($mucCallParticipants.get()).toEqual({
      "chan@muc.test": ["alice"],
    });
    expect($mucCallParticipantOwners.get()).toEqual({
      "chan@muc.test": [{ nick: "alice", realJid: "alice@waddle.test/phone" }],
    });
  });

  test("clears ownerless retained Muji presence after hard reload", async () => {
    applyMucCallPresence({
      from: "chan@muc.test/alice",
      muji: { preparing: false, active: true },
    });
    const sender: RawIqSender = {
      update_muji_presence: mock(async () => undefined),
    };

    await leaveRetainedMucCallAction({
      roomJid: "chan@muc.test",
      getSender: () => sender,
      getSelfNick: () => "alice",
      getSelfFullJid: () => "alice@waddle.test/web",
    });

    expect(sender.update_muji_presence).toHaveBeenCalledWith(
      "chan@muc.test",
      "alice",
      false,
      false,
      false,
      { handRaised: false, muted: false }, // leaving clears all in-call state (#1030)
    );
    expect($mucCallParticipants.get()).toEqual({});
    expect($mucCallParticipantOwners.get()).toEqual({});
  });

  test("terminates the cached Muji Jingle session after clearing retained presence", async () => {
    const now = new Date();
    const sent: unknown[][] = [];
    applyMucCallPresence({
      from: "chan@muc.test/alice",
      muc_jid: "alice@waddle.test/web",
      muji: { preparing: false, active: true },
    });
    rememberMucCallSession({
      roomJid: "chan@muc.test",
      sid: "muc-recovered-live",
      selfFullJid: "alice@waddle.test/web",
      now,
    });
    const sender: RawIqSender = {
      update_muji_presence: mock(async (...args) => {
        sent.push(["presence", ...args]);
      }),
      send_muji_session_terminate: mock(async (...args) => {
        sent.push(["terminate", ...args]);
      }),
    };

    await expect(leaveRetainedMucCallAction({
      roomJid: "chan@muc.test",
      getSender: () => sender,
      getSelfNick: () => "alice",
      getSelfFullJid: () => "alice@waddle.test/web",
    })).resolves.toBe(true);

    expect(sent).toEqual([
      ["presence", "chan@muc.test", "alice", false, false, false, { handRaised: false, muted: false }],
      ["terminate", "chan@muc.test", "muc-recovered-live"],
    ]);
    expect($mucCallParticipants.get()).toEqual({});
    expect(readMucCallSession({
      roomJid: "chan@muc.test",
      selfFullJid: "alice@waddle.test/web",
      now,
    })).toBeNull();
  });

  test("reports cached Muji terminate failure after retained presence is cleared", async () => {
    const now = new Date();
    applyMucCallPresence({
      from: "chan@muc.test/alice",
      muc_jid: "alice@waddle.test/web",
      muji: { preparing: false, active: true },
    });
    rememberMucCallSession({
      roomJid: "chan@muc.test",
      sid: "muc-retry-live",
      selfFullJid: "alice@waddle.test/web",
      media: { audio: true, video: true },
      now,
    });
    const sender: RawIqSender = {
      update_muji_presence: mock(async () => undefined),
      send_muji_session_terminate: mock(async () => {
        throw new Error("simulated Muji terminate failure");
      }),
    };

    const result = await leaveRetainedMucCallAction({
      roomJid: "chan@muc.test",
      getSender: () => sender,
      getSelfNick: () => "alice",
      getSelfFullJid: () => "alice@waddle.test/web",
    });

    expect(result).toBe(false);
    expect(sender.update_muji_presence).toHaveBeenCalledWith(
      "chan@muc.test",
      "alice",
      false,
      false,
      false,
      { handRaised: false, muted: false }, // leaving clears all in-call state (#1030)
    );
    expect(sender.send_muji_session_terminate).toHaveBeenCalledWith(
      "chan@muc.test",
      "muc-retry-live",
    );
    expect($mucCallParticipants.get()).toEqual({});
    expect(readMucCallSession({
      roomJid: "chan@muc.test",
      selfFullJid: "alice@waddle.test/web",
      now,
    })).toMatchObject({ sid: "muc-retry-live" });
    expect($mucCallTerminatePendingSessions.get()).toEqual({
      "chan@muc.test\u0000muc-retry-live\u0000alice@waddle.test/web": {
        roomJid: "chan@muc.test",
        sid: "muc-retry-live",
        selfFullJid: "alice@waddle.test/web",
        media: { audio: true, video: true },
        updatedAt: expect.any(String),
        terminatePending: true,
      },
    });
    expect($lastCallError.get()).toContain("simulated Muji terminate failure");
  });

  test("keeps retained Muji state visible when the leave marker fails to send", async () => {
    applyMucCallPresence({
      from: "chan@muc.test/alice",
      muc_jid: "alice@waddle.test/web",
      muji: { preparing: false, active: true },
    });
    const sender: RawIqSender = {
      update_muji_presence: mock(async () => {
        throw new Error("simulated Muji leave failure");
      }),
    };

    const result = await leaveRetainedMucCallAction({
      roomJid: "chan@muc.test",
      getSender: () => sender,
      getSelfNick: () => "alice",
      getSelfFullJid: () => "alice@waddle.test/web",
    });

    expect(result).toBe(false);
    expect(sender.update_muji_presence).toHaveBeenCalledWith(
      "chan@muc.test",
      "alice",
      false,
      false,
      false,
      { handRaised: false, muted: false }, // leaving clears all in-call state (#1030)
    );
    expect($mucCallParticipants.get()).toEqual({
      "chan@muc.test": ["alice"],
    });
    expect($mucCallParticipantOwners.get()).toEqual({
      "chan@muc.test": [{ nick: "alice", realJid: "alice@waddle.test/web" }],
    });
    expect($lastCallError.get()).toContain("simulated Muji leave failure");
  });

  test("remembering a fresh Muji session clears stale pending cleanup for the same room", () => {
    markMucCallSessionTerminatePending({
      roomJid: "chan@muc.test",
      sid: "muc-stale-video",
      selfFullJid: "alice@waddle.test/web",
      media: { audio: true, video: true },
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    rememberMucCallSession({
      roomJid: "chan@muc.test",
      sid: "muc-fresh-voice",
      selfFullJid: "alice@waddle.test/web",
      media: { audio: true, video: false },
      now: new Date("2026-05-26T12:01:00.000Z"),
    });

    expect($mucCallTerminatePendingSessions.get()).toEqual({});
    expect(readMucCallSession({
      roomJid: "chan@muc.test",
      selfFullJid: "alice@waddle.test/web",
      now: new Date("2026-05-26T12:01:00.000Z"),
    })).toMatchObject({
      sid: "muc-fresh-voice",
      media: { audio: true, video: false },
    });
  });
});

describe("1:1 call event wiring", () => {
  test("self-originated sibling propose does not surface as an incoming call", async () => {
    const sender = mockSender();
    const events = wireClientEvents(sender);

    events.emitCall({
      kind: "propose",
      from: "alice@waddle.test/phone",
      to: "bob@waddle.test/desktop",
      sid: "sibling-started",
      media: audioVideo,
    });
    await flushCallSideEffects();

    expect($callState.get()).toEqual({ phase: "idle" });
    expect(sender.send_call_reject).not.toHaveBeenCalled();
  });

  test("self-originated sibling reject does not end this resource's active DM call", async () => {
    const events = wireClientEvents();
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "shared-call",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });

    events.emitCall({
      kind: "reject",
      from: "alice@waddle.test/phone",
      to: "bob@waddle.test/desktop",
      sid: "shared-call",
      reason: "decline",
    });
    await flushCallSideEffects();

    expect($callState.get()).toEqual({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "shared-call",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
  });

  test("self-originated sibling reject clears this resource's incoming DM ring", async () => {
    const telemetryEvents: Array<{ name: string; attributes?: Record<string, string> }> = [];
    __setFaroForTesting({
      api: {
        pushEvent: (name: string, attributes?: Record<string, string>) => {
          telemetryEvents.push({ name, attributes });
        },
      },
    } as never);
    const sender = mockSender();
    const events = wireClientEvents(sender);
    events.emitCall({
      kind: "propose",
      from: "bob@waddle.test/desktop",
      sid: "shared-call",
      media: audioVideo,
    });

    events.emitCall({
      kind: "reject",
      from: "alice@waddle.test/phone",
      to: "bob@waddle.test/desktop",
      sid: "shared-call",
      reason: "decline",
    });
    await flushCallSideEffects();

    expect($callState.get()).toEqual({
      phase: "ended",
      sid: "shared-call",
      reason: "reject",
    });
    expect(sender.send_call_reject).not.toHaveBeenCalled();
    expect(telemetryEvents).toEqual([{
      name: "chat.call.lifecycle",
      attributes: expect.objectContaining({
        setup_outcome: "declined",
        end_reason: "hangup",
        call_kind: "dm",
      }),
    }]);
  });

  test("self-originated sibling finish records a local active-call hangup", async () => {
    const telemetryEvents: Array<{ name: string; attributes?: Record<string, string> }> = [];
    __setFaroForTesting({
      api: {
        pushEvent: (name: string, attributes?: Record<string, string>) => {
          telemetryEvents.push({ name, attributes });
        },
      },
    } as never);
    const events = wireClientEvents();
    beginOutgoingCall("bob@waddle.test", "shared-active-call", audioVideo);
    events.emitCall({
      kind: "session-accept",
      from: "bob@waddle.test/desktop",
      sid: "shared-active-call",
      media: audioVideo,
      join,
    });
    events.emitCall({
      kind: "finish",
      from: "alice@waddle.test/phone",
      to: "bob@waddle.test/desktop",
      sid: "shared-active-call",
      reason: "success",
    });
    await flushCallSideEffects();

    expect(telemetryEvents).toEqual([{
      name: "chat.call.lifecycle",
      attributes: expect.objectContaining({
        setup_outcome: "accepted",
        end_reason: "hangup",
        call_kind: "dm",
      }),
    }]);
  });

  test("real on_call propose event applies lower-sid tie-break and surfaces incoming UI", async () => {
    const send_call_retract_tie_break = mock(async () => undefined);
    const events = wireClientEvents({ send_call_retract_tie_break });

    beginOutgoingCall("bob@waddle.test", "z-outgoing", audioVideo);
    events.emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      sid: "a-incoming",
      media: audioVideo,
    });
    await flushCallSideEffects();

    expect(send_call_retract_tie_break).toHaveBeenCalledTimes(1);
    expect(send_call_retract_tie_break).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "z-outgoing",
    );
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "bob@waddle.test/phone",
      sid: "a-incoming",
      media: audioVideo,
    });
  });

  test("real on_call propose event applies higher-sid tie-break without changing outgoing UI", async () => {
    const send_call_reject_tie_break = mock(async () => undefined);
    const events = wireClientEvents({ send_call_reject_tie_break });

    beginOutgoingCall("bob@waddle.test", "a-outgoing", audioVideo);
    events.emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      sid: "z-incoming",
      media: audioVideo,
    });
    await flushCallSideEffects();

    expect(send_call_reject_tie_break).toHaveBeenCalledTimes(1);
    expect(send_call_reject_tie_break).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "z-incoming",
    );
    expect($callState.get()).toEqual({
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "a-outgoing",
      media: audioVideo,
    });
  });

  test("real on_call proceed event cancels outgoing ring timeout", async () => {
    const sender = mockSender();
    const events = wireClientEvents(sender);

    beginOutgoingCall("bob@waddle.test", "c-accepted", audioVideo);
    scheduleOutgoingTimeout(sender, "c-accepted", 10);
    events.emitCall({
      kind: "proceed",
      from: "bob@waddle.test/phone",
      sid: "c-accepted",
    });
    await new Promise((resolve) => setTimeout(resolve, 25));

    expect(sender.send_call_retract).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "c-accepted",
      media: audioVideo,
    });
  });

  test("real on_call proceed event times out if no session-accept follows", async () => {
    const restoreTimeout = setSessionAcceptTimeoutMsForTests(10);
    try {
      const sender = mockSender();
      const events = wireClientEvents(sender);

      beginOutgoingCall(
        "bob@waddle.test",
        "c-no-accept",
        audioVideo,
        "alice@waddle.test/web",
      );
      scheduleOutgoingTimeout(sender, "c-no-accept", 100);
      events.emitCall({
        kind: "proceed",
        from: "bob@waddle.test/phone",
        sid: "c-no-accept",
      });
      await flushCallSideEffects();
      await new Promise((resolve) => setTimeout(resolve, 25));

      expect(sender.send_call_retract).not.toHaveBeenCalled();
      expect(sender.send_call_session_initiate).toHaveBeenCalledTimes(1);
      const initiateCalls = (
        sender.send_call_session_initiate as unknown as {
          mock: { calls: unknown[][] };
        }
      ).mock.calls;
      const initiatorFullJid = String(initiateCalls[0]?.[1] ?? "");
      expect(initiateCalls[0]?.[0]).toBe("bob@waddle.test/phone");
      expect(initiatorFullJid.startsWith("alice@waddle.test/web")).toBe(true);
      expect(initiateCalls[0]?.[2]).toBe("c-no-accept");
      expect(initiateCalls[0]?.[3]).toBe(true);
      expect(initiateCalls[0]?.[4]).toBe(true);
      expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
        "bob@waddle.test/phone",
        "c-no-accept",
        "timeout",
      );
      expect($dmCallOutcomeAnchor.get()).toMatchObject({
        peerBareJid: "bob@waddle.test",
        sid: "c-no-accept",
        outcome: "no-answer",
      });
      expect($callState.get()).toEqual({
        phase: "ended",
        sid: "c-no-accept",
        reason: "timeout",
      });
    } finally {
      restoreTimeout();
    }
  });

  test("real on_call session-accept cancels the post-proceed timeout", async () => {
    const restoreTimeout = setSessionAcceptTimeoutMsForTests(10);
    try {
      const sender = mockSender();
      const events = wireClientEvents(sender);

      beginOutgoingCall(
        "bob@waddle.test",
        "c-accepts",
        audioVideo,
        "alice@waddle.test/web",
      );
      events.emitCall({
        kind: "proceed",
        from: "bob@waddle.test/phone",
        sid: "c-accepts",
      });
      await flushCallSideEffects();
      events.emitCall({
        kind: "session-accept",
        from: "bob@waddle.test/phone",
        sid: "c-accepts",
        media: audioVideo,
        join,
      });
      await new Promise((resolve) => setTimeout(resolve, 25));

      expect(sender.send_call_session_terminate).not.toHaveBeenCalled();
      expect($callState.get()).toEqual({
        phase: "active",
        peer: "bob@waddle.test/phone",
        sid: "c-accepts",
        media: audioVideo,
        join,
        kind: "dm",
        initiator: "alice@waddle.test/web",
      });
    } finally {
      restoreTimeout();
    }
  });

  test("real on_call active same-bare propose migrates the existing session", async () => {
    const sender = mockSender();
    const events = wireClientEvents(sender);

    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/phone",
      sid: "old-call",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    events.emitCall({
      kind: "propose",
      from: "bob@waddle.test/tablet",
      sid: "new-call",
      media: audioVideo,
    });
    await flushCallSideEffects();
    await flushCallSideEffects();

    expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "old-call",
      "expired",
    );
    expect(sender.send_call_finish_migrated).toHaveBeenCalledWith(
      "bob@waddle.test/tablet",
      "old-call",
      "new-call",
    );
    expect(sender.send_call_proceed).toHaveBeenCalledWith(
      "bob@waddle.test/tablet",
      "new-call",
    );
    await flushCallSideEffects();
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "bob@waddle.test/tablet",
      sid: "new-call",
      media: audioVideo,
    });
  });

  test("real on_call active migration sends markers before old cleanup completes", async () => {
    const sender = mockSender();
    sender.send_call_session_terminate = mock(async () => {
      await new Promise(() => undefined);
    });
    const events = wireClientEvents(sender);

    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/phone",
      sid: "old-call",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    events.emitCall({
      kind: "propose",
      from: "bob@waddle.test/tablet",
      sid: "new-call",
      media: audioVideo,
    });
    await flushCallSideEffects();

    expect(sender.send_call_finish_migrated).toHaveBeenCalledWith(
      "bob@waddle.test/tablet",
      "old-call",
      "new-call",
    );
    expect(sender.send_call_proceed).toHaveBeenCalledWith(
      "bob@waddle.test/tablet",
      "new-call",
    );
    expect(sender.send_call_session_terminate).toHaveBeenCalledWith(
      "bob@waddle.test/phone",
      "old-call",
      "expired",
    );
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "bob@waddle.test/tablet",
      sid: "new-call",
      media: audioVideo,
    });
  });

  test("real on_call active migration proceeds even when old cleanup fails", async () => {
    const sender = mockSender();
    sender.send_call_session_terminate = mock(async () => {
      throw new Error("terminate failed");
    });
    const events = wireClientEvents(sender);

    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/phone",
      sid: "old-call",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    events.emitCall({
      kind: "propose",
      from: "bob@waddle.test/tablet",
      sid: "new-call",
      media: audioVideo,
    });
    await flushCallSideEffects();

    expect(sender.send_call_finish_migrated).toHaveBeenCalledWith(
      "bob@waddle.test/tablet",
      "old-call",
      "new-call",
    );
    expect(sender.send_call_proceed).toHaveBeenCalledWith(
      "bob@waddle.test/tablet",
      "new-call",
    );
    expect($callState.get()).toEqual({
      phase: "incoming",
      from: "bob@waddle.test/tablet",
      sid: "new-call",
      media: audioVideo,
    });
    expect($lastCallError.get()).toBe("terminate failed");
  });

  test("real on_call active migration does not leave old active UI when proceed fails", async () => {
    const sender = mockSender();
    sender.send_call_proceed = mock(async () => {
      throw new Error("proceed failed");
    });
    const events = wireClientEvents(sender);

    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/phone",
      sid: "old-call",
      media: audioVideo,
      join,
      kind: "dm",
      initiator: "alice@waddle.test/web",
    });
    events.emitCall({
      kind: "propose",
      from: "bob@waddle.test/tablet",
      sid: "new-call",
      media: audioVideo,
    });
    await flushCallSideEffects();

    expect(sender.send_call_finish_migrated).toHaveBeenCalledWith(
      "bob@waddle.test/tablet",
      "old-call",
      "new-call",
    );
    expect(sender.send_call_session_terminate).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({
      phase: "ended",
      sid: "old-call",
      reason: "error",
    });
    expect($lastCallError.get()).toBe("proceed failed");
  });

  test("real on_call tie-break reject event surfaces expired rather than normal declined", () => {
    const events = wireClientEvents();

    beginOutgoingCall("bob@waddle.test", "c-lost", audioVideo);
    events.emitCall({
      kind: "reject",
      from: "bob@waddle.test/phone",
      sid: "c-lost",
      reason: "expired",
      tieBreak: true,
    });

    expect($callState.get()).toEqual({
      phase: "ended",
      sid: "c-lost",
      reason: "expired",
    });
  });
});

describe("MUC group call", () => {
  test("legacy presence event path updates the room call indicator from real presence events", () => {
    const events = wireEmitterPresenceEvents();

    events.emitPresence({
      from: "Chan@MUC.Test/alice",
      presence_type: "available",
      muji: { preparing: false, active: true },
    });

    expect($mucCallParticipants.get()).toEqual({
      "chan@muc.test": ["alice"],
    });

    events.emitPresence({
      from: "Chan@MUC.Test/alice",
      presence_type: "available",
    });

    expect($mucCallParticipants.get()).toEqual({});
  });

  test("BrowserXmppClient.disconnect clears group-call indicators seeded by presence events", async () => {
    const disconnect = mock(async () => undefined);
    const events = wireClientEvents({ disconnect } as unknown as Partial<CallWireSender>);
    const client = events.client as unknown as {
      disconnect: () => Promise<void>;
    };

    events.emitPresence({
      from: "chan@muc.test/alice",
      presence_type: "available",
      muji: { preparing: false, active: true },
    });
    expect($mucCallParticipants.get()).toEqual({
      "chan@muc.test": ["alice"],
    });

    await client.disconnect();

    expect(disconnect).toHaveBeenCalledTimes(1);
    expect($mucCallParticipants.get()).toEqual({});
  });

  test("BrowserXmppClient ignores stale call and presence events after disconnect", async () => {
    const disconnect = mock(async () => undefined);
    const events = wireClientEvents({ disconnect } as unknown as Partial<CallWireSender>);
    const client = events.client as unknown as {
      disconnect: () => Promise<void>;
    };

    await client.disconnect();

    events.emitPresence({
      from: "chan@muc.test/alice",
      presence_type: "available",
      muji: { preparing: false, active: true },
    });
    events.emitCall({
      kind: "propose",
      from: "bob@waddle.test/phone",
      sid: "late-propose",
      media: audioVideo,
    });

    expect($mucCallParticipants.get()).toEqual({});
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("beginMucCall sets active(kind: 'muc') after awaiting the separate session-accept (XEP-0166 §6.3)", async () => {
    const events = wireClientEvents();
    const expectedJoin: LiveKitJoin = {
      url: "wss://livekit.test",
      room: "chan@muc.test",
      identity: "alice@waddle.test/web",
      token: "jwt.payload.sig",
    };
    const selfFullJid = "alice@waddle.test/web";
    const update_muji_presence = mockUpdateMujiPresenceWithEcho(
      events.emitPresence,
      selfFullJid,
    );
    const sender = {
      send_muji_session_initiate:
        mockSendMujiSessionInitiateWithAccept(expectedJoin, events.emitCall),
      update_muji_presence,
    };
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    await beginMucCall(
      sender,
      "chan@muc.test",
      audioVideo,
      "alice",
      undefined,
      selfFullJid,
    );
    const state = $callState.get();
    expect(state).toMatchObject({
      phase: "active",
      peer: "chan@muc.test",
      media: audioVideo,
      join: expectedJoin,
      kind: "muc",
      selfNick: "alice",
      selfFullJid,
    });
    expect(typeof (state as { sid?: unknown }).sid).toBe("string");
    expect((state as { sid: string }).sid).not.toBe("chan@muc.test");
    expect(sender.send_muji_session_initiate).toHaveBeenCalledWith(
      "chan@muc.test",
      expect.any(String),
      true, // audioVideo.video
    );
  });

  test("beginMucCall only resolves from the expected mixer and room event", async () => {
    const events = wireClientEvents();
    const forgedJoin: LiveKitJoin = {
      url: "wss://evil-livekit.test",
      room: "chan@muc.test",
      identity: "mallory@waddle.test/desktop",
      token: "evil.jwt",
    };
    const wrongRoomJoin: LiveKitJoin = {
      url: "wss://livekit.test",
      room: "other@muc.test",
      identity: "alice@waddle.test/web",
      token: "wrong.room.jwt",
    };
    const expectedJoin: LiveKitJoin = {
      url: "wss://livekit.test",
      room: "chan@muc.test",
      identity: "alice@waddle.test/web",
      token: "real.jwt",
    };
    const update_muji_presence = mockUpdateMujiPresenceWithEcho(events.emitPresence);
    const send_muji_session_initiate = mock(async (_roomJid: string, sid: string) => {
      queueMicrotask(() => {
        events.emitCall({
          kind: "session-accept",
          from: "mallory@waddle.test/desktop",
          sid,
          media: audioVideo,
          join: forgedJoin,
        });
        events.emitCall({
          kind: "session-accept",
          from: "calls.waddle.test",
          sid,
          media: audioVideo,
          join: wrongRoomJoin,
        });
        events.emitCall({
          kind: "session-accept",
          from: "calls.waddle.test",
          sid,
          media: audioVideo,
          join: expectedJoin,
        });
      });
    });
    const { beginMucCall } = await import("../src/lib/calls/call-store");

    await beginMucCall(
      { send_muji_session_initiate, update_muji_presence },
      "chan@muc.test",
      audioVideo,
      "alice",
      "calls.waddle.test",
    );

    expect($callState.get()).toMatchObject({
      phase: "active",
      peer: "chan@muc.test",
      media: audioVideo,
      join: expectedJoin,
      kind: "muc",
      selfNick: "alice",
    });
  });

  test("beginMucCall rejects a second start while preparing/accept events are pending", async () => {
    const events = wireClientEvents();
    const expectedJoin: LiveKitJoin = {
      url: "wss://livekit.test",
      room: "chan@muc.test",
      identity: "alice@waddle.test/web",
      token: "jwt.payload.sig",
    };
    let emitPreparingEcho: (() => void) | null = null;
    let emitAccept: (() => void) | null = null;
    const update_muji_presence = mock(
      async (
        roomJid: string,
        nick: string,
        active: boolean,
        preparing: boolean,
        _video: boolean,
      ) => {
        if (preparing) {
          emitPreparingEcho = () => events.emitPresence({
            from: `${roomJid}/${nick}`,
            presence_type: "available",
            muji: { preparing: true, active: false },
          });
        }
        if (active) {
          events.emitPresence({
            from: `${roomJid}/${nick}`,
            presence_type: "available",
            muji: { preparing: false, active: true },
          });
        }
      },
    );
    const send_muji_session_initiate = mock(async (_roomJid: string, sid: string) => {
      emitAccept = () => events.emitCall({
        kind: "session-accept",
        from: "calls.waddle.test",
        sid,
        media: audioVideo,
        join: expectedJoin,
      });
    });
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    const sender = {
      send_muji_session_initiate,
      update_muji_presence,
    };

    const first = beginMucCall(sender, "chan@muc.test", audioVideo, "alice");
    const secondError = await beginMucCall(
      sender,
      "chan@muc.test",
      audioVideo,
      "alice",
    ).then(
      () => null,
      (err) => err,
    );

    expect(secondError).toBeInstanceOf(Error);
    expect(update_muji_presence).toHaveBeenCalledTimes(1);
    const pendingState = $callState.get();
    expect(pendingState).toMatchObject({
      phase: "muc-pending",
      peer: "chan@muc.test",
      media: audioVideo,
      kind: "muc",
      selfNick: "alice",
    });
    expect(typeof (pendingState as { attemptId?: unknown }).attemptId).toBe("string");

    emitPreparingEcho?.();
    await flushCallSideEffects();
    expect(update_muji_presence).toHaveBeenCalledTimes(2);
    expect(send_muji_session_initiate).toHaveBeenCalledTimes(1);
    emitAccept?.();
    await first;

    expect($callState.get()).toMatchObject({
      phase: "active",
      peer: "chan@muc.test",
      media: audioVideo,
      join: expectedJoin,
      kind: "muc",
      selfNick: "alice",
    });
  });

  test("beginMucCall waits for other preparing participants before content presence", async () => {
    const events = wireClientEvents();
    const expectedJoin: LiveKitJoin = {
      url: "wss://livekit.test",
      room: "chan@muc.test",
      identity: "alice@waddle.test/web",
      token: "jwt.payload.sig",
    };
    events.emitPresence({
      from: "chan@muc.test/bob",
      presence_type: "available",
      muji: { preparing: true, active: false },
    });
    const update_muji_presence = mockUpdateMujiPresenceWithEcho(events.emitPresence);
    const send_muji_session_initiate =
      mockSendMujiSessionInitiateWithAccept(expectedJoin, events.emitCall);
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    const pending = beginMucCall(
      { send_muji_session_initiate, update_muji_presence },
      "chan@muc.test",
      audioVideo,
      "alice",
    );

    await flushCallSideEffects();
    expect(update_muji_presence).toHaveBeenCalledTimes(1);
    expect(send_muji_session_initiate).not.toHaveBeenCalled();

    events.emitPresence({
      from: "chan@muc.test/bob",
      presence_type: "available",
    });
    await pending;

    expect(update_muji_presence).toHaveBeenNthCalledWith(
      2,
      "chan@muc.test",
      "alice",
      true,
      false,
      true,
      // audioVideo fixture captures audio, so !audio = false (#1030)
      { handRaised: false, muted: false },
    );
    expect(send_muji_session_initiate).toHaveBeenCalledTimes(1);
    expect($callState.get()).toMatchObject({
      phase: "active",
      peer: "chan@muc.test",
      media: audioVideo,
      join: expectedJoin,
      kind: "muc",
      selfNick: "alice",
    });
  });

  test("group-call button action ignores a second click while start is pending", async () => {
    const events = wireClientEvents();
    const expectedJoin: LiveKitJoin = {
      url: "wss://livekit.test",
      room: "chan@muc.test",
      identity: "alice@waddle.test/web",
      token: "jwt.payload.sig",
    };
    let starting = false;
    let emitPreparingEcho: (() => void) | null = null;
    let emitAccept: (() => void) | null = null;
    const update_muji_presence = mock(
      async (
        roomJid: string,
        nick: string,
        active: boolean,
        preparing: boolean,
        _video: boolean,
      ) => {
        if (preparing) {
          emitPreparingEcho = () => events.emitPresence({
            from: `${roomJid}/${nick}`,
            presence_type: "available",
            muji: { preparing: true, active: false },
          });
        }
        if (active) {
          events.emitPresence({
            from: `${roomJid}/${nick}`,
            presence_type: "available",
            muji: { preparing: false, active: true },
          });
        }
      },
    );
    const send_muji_session_initiate = mock(async (_roomJid: string, sid: string) => {
      emitAccept = () => events.emitCall({
        kind: "session-accept",
        from: "calls.waddle.test",
        sid,
        media: audioVideo,
        join: expectedJoin,
      });
    });
    const sender = {
      send_muji_session_initiate,
      update_muji_presence,
    };
    const run = () => startMucCallAction({
      roomJid: "chan@muc.test",
      media: audioVideo,
      isBusy: () => starting || !["idle", "ended"].includes($callState.get().phase),
      setStarting: (next) => {
        starting = next;
      },
      getSender: () => sender,
      getSelfNick: () => "alice",
    });

    const first = run();
    await flushCallSideEffects();
    expect(starting).toBe(true);
    expect($callState.get().phase).toBe("muc-pending");
    expect(await run()).toBe(false);
    expect(update_muji_presence).toHaveBeenCalledTimes(1);

    emitPreparingEcho?.();
    await flushCallSideEffects();
    expect(send_muji_session_initiate).toHaveBeenCalledTimes(1);
    emitAccept?.();
    expect(await first).toBe(true);
    expect(starting).toBe(false);
    expect($callState.get()).toMatchObject({
      phase: "active",
      peer: "chan@muc.test",
      media: audioVideo,
      join: expectedJoin,
      kind: "muc",
      selfNick: "alice",
    });
  });

  test("group-call button reports a missing XMPP sender as one failed MUC preflight", async () => {
    const telemetryEvents: Array<{ name: string; attributes?: Record<string, string> }> = [];
    __setFaroForTesting({
      api: {
        pushEvent: (name: string, attributes?: Record<string, string>) => {
          telemetryEvents.push({ name, attributes });
        },
      },
    } as never);

    const started = await startMucCallAction({
      roomJid: "chan@muc.test",
      media: audioVideo,
      isBusy: () => false,
      setStarting: () => undefined,
      getSender: () => null,
      getSelfNick: () => "alice",
    });

    expect(started).toBe(false);
    expect($callState.get()).toEqual({ phase: "idle" });
    expect(telemetryEvents).toEqual([{
      name: "chat.call.lifecycle",
      attributes: expect.objectContaining({
        setup_outcome: "failed",
        end_reason: "error",
        call_kind: "muc",
      }),
    }]);
  });

  test("BrowserXmppClient.disconnect rejects a pending MUC preparing wait", async () => {
    const events = wireClientEvents();
    const update_muji_presence = mock(async () => undefined);
    const send_muji_session_terminate = mock(async () => undefined);
    const disconnect = mock(async () => undefined);
    const client = events.client as unknown as {
      xmpp: {
        update_muji_presence: typeof update_muji_presence;
        send_muji_session_terminate: typeof send_muji_session_terminate;
        disconnect: typeof disconnect;
      };
      disconnect: () => Promise<void>;
    };
    client.xmpp = {
      update_muji_presence,
      send_muji_session_terminate,
      disconnect,
    };
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    const pending = beginMucCall(
      client.xmpp,
      "chan@muc.test",
      audioVideo,
      "alice",
    );
    await flushCallSideEffects();
    expect($callState.get().phase).toBe("muc-pending");

    await client.disconnect();

    await expect(pending).rejects.toThrow("cancelled");
    expect(disconnect).toHaveBeenCalledTimes(1);
    expect(send_muji_session_terminate).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("BrowserXmppClient.disconnect rejects a pending MUC session-accept wait", async () => {
    const events = wireClientEvents();
    const update_muji_presence = mockUpdateMujiPresenceWithEcho(events.emitPresence);
    const send_muji_session_initiate = mock(async () => undefined);
    const send_muji_session_terminate = mock(async () => undefined);
    const disconnect = mock(async () => undefined);
    const client = events.client as unknown as {
      xmpp: {
        update_muji_presence: typeof update_muji_presence;
        send_muji_session_initiate: typeof send_muji_session_initiate;
        send_muji_session_terminate: typeof send_muji_session_terminate;
        disconnect: typeof disconnect;
      };
      disconnect: () => Promise<void>;
    };
    Object.assign(client.xmpp, {
      update_muji_presence,
      send_muji_session_initiate,
      send_muji_session_terminate,
      disconnect,
    });
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    const pending = beginMucCall(
      client.xmpp,
      "chan@muc.test",
      audioVideo,
      "alice",
    );
    await flushCallSideEffects();
    await flushCallSideEffects();
    expect($callState.get().phase).toBe("muc-pending");
    expect(send_muji_session_initiate).toHaveBeenCalledTimes(1);

    await client.disconnect();

    await expect(pending).rejects.toThrow("cancelled");
    expect(disconnect).toHaveBeenCalledTimes(1);
    const attemptSid = firstMockCallArg(send_muji_session_initiate, 1);
    expect(attemptSid).not.toBe("chan@muc.test");
    expect(send_muji_session_terminate).toHaveBeenCalledWith(
      "chan@muc.test",
      attemptSid,
    );
    expect(update_muji_presence).toHaveBeenLastCalledWith(
      "chan@muc.test",
      "alice",
      false,
      false,
      false,
      { handRaised: false, muted: false }, // leaving clears all in-call state (#1030)
    );
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("real disconnected event rejects pending MUC session-accept wait without SFU teardown", async () => {
    const events = wireClientEvents();
    const update_muji_presence = mockUpdateMujiPresenceWithEcho(events.emitPresence);
    const send_muji_session_initiate = mock(async () => undefined);
    const send_muji_session_terminate = mock(async () => undefined);
    const client = events.client as unknown as {
      xmpp: {
        update_muji_presence: typeof update_muji_presence;
        send_muji_session_initiate: typeof send_muji_session_initiate;
        send_muji_session_terminate: typeof send_muji_session_terminate;
      };
      disconnect: () => Promise<void>;
    };
    Object.assign(client.xmpp, {
      update_muji_presence,
      send_muji_session_initiate,
      send_muji_session_terminate,
    });
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    const pending = beginMucCall(
      client.xmpp,
      "chan@muc.test",
      audioVideo,
      "alice",
    );
    await flushCallSideEffects();
    await flushCallSideEffects();
    expect(send_muji_session_initiate).toHaveBeenCalledTimes(1);

    events.emitDisconnected();

    await expect(pending).rejects.toThrow("cancelled");
    expect(send_muji_session_terminate).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
    await client.disconnect();
  });

  test("replacement MUC start is not rolled back by a stale disconnected attempt", async () => {
    const firstEvents = wireClientEvents();
    const secondEvents = wireClientEvents();
    const expectedJoin: LiveKitJoin = {
      url: "wss://livekit.test",
      room: "chan@muc.test",
      identity: "alice@waddle.test/web",
      token: "replacement.jwt",
    };
    const staleJoin: LiveKitJoin = {
      url: "wss://old-livekit.test",
      room: "chan@muc.test",
      identity: "alice@waddle.test/web",
      token: "stale.jwt",
    };
    let staleSid = "";
    const firstSender = {
      send_muji_session_initiate: mock(async (_roomJid: string, sid: string) => {
        staleSid = sid;
      }),
      update_muji_presence: mockUpdateMujiPresenceWithEcho(firstEvents.emitPresence),
    };
    const secondSender = {
      send_muji_session_initiate: mock(async (_roomJid: string, sid: string) => {
        queueMicrotask(() => {
          secondEvents.emitCall({
            kind: "session-accept",
            from: "calls.waddle.test",
            sid: staleSid,
            media: audioVideo,
            join: staleJoin,
          });
          secondEvents.emitCall({
            kind: "session-accept",
            from: "calls.waddle.test",
            sid,
            media: audioVideo,
            join: expectedJoin,
          });
        });
      }),
      update_muji_presence: mockUpdateMujiPresenceWithEcho(secondEvents.emitPresence),
    };
    const { beginMucCall } = await import("../src/lib/calls/call-store");

    const first = beginMucCall(
      firstSender,
      "chan@muc.test",
      audioVideo,
      "alice",
      "calls.waddle.test",
    );
    await flushCallSideEffects();
    await flushCallSideEffects();
    expect(firstSender.send_muji_session_initiate).toHaveBeenCalledTimes(1);

    firstEvents.emitDisconnected();
    const second = beginMucCall(
      secondSender,
      "chan@muc.test",
      audioVideo,
      "alice",
      "calls.waddle.test",
    );

    await expect(first).rejects.toThrow("cancelled");
    await second;
    expect($callState.get()).toMatchObject({
      phase: "active",
      peer: "chan@muc.test",
      media: audioVideo,
      join: expectedJoin,
      kind: "muc",
      selfNick: "alice",
    });
  });

  test("beginMucCall with a nick implements the XEP-0272 §Joining two-phase preparing→content flow", async () => {
    const events = wireClientEvents();
    const send_muji_session_initiate = mockSendMujiSessionInitiateWithAccept({
      url: "wss://livekit.test",
      room: "chan@muc.test",
      identity: "alice@waddle.test/web",
      token: "jwt.payload.sig",
    }, events.emitCall);
    const update_muji_presence = mockUpdateMujiPresenceWithEcho(events.emitPresence);
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    await beginMucCall(
      {
        send_muji_session_initiate,
        update_muji_presence,
      } as unknown as Parameters<typeof beginMucCall>[0],
      "chan@muc.test",
      audioVideo,
      "alice",
    );
    // Two-phase flow:
    //   1. preparing  → `<muji><preparing/></muji>`
    //   2. active content → `<muji><content .../></muji>`
    //   3. session-initiate IQ round-trip after content presence
    expect(update_muji_presence).toHaveBeenCalledTimes(2);
    expect(update_muji_presence).toHaveBeenNthCalledWith(
      1,
      "chan@muc.test",
      "alice",
      false, // active
      true, // preparing
      false, // video — irrelevant in preparing phase
      // in-call state isn't advertised before joining (#1030)
      { handRaised: false, muted: false },
    );
    expect(update_muji_presence).toHaveBeenNthCalledWith(
      2,
      "chan@muc.test",
      "alice",
      true, // active
      false, // preparing
      true, // video (audioVideo fixture has video=true)
      // audioVideo fixture captures audio, so !audio = false (#1030)
      { handRaised: false, muted: false },
    );
    expect(send_muji_session_initiate).toHaveBeenCalledTimes(1);
  });

  test("tearDownActiveCall still clears the Muji presence when sendMujiSessionTerminate throws", async () => {
    // Regression guard: a stale wasm bundle without
    // `send_muji_session_terminate` makes the call throw. The
    // presence cleanup MUST still run — otherwise the user's
    // `<muji/>` advertisement lingers until the XMPP session
    // disconnects, exactly the bug this teardown path exists to fix.
    const events = wireClientEvents();
    const update_muji_presence = mockUpdateMujiPresenceWithEcho(events.emitPresence);
    $callState.set({
      phase: "active",
      peer: "chan@muc.test",
      sid: "muc-attempt",
      media: audioVideo,
      join,
      kind: "muc",
      selfNick: "alice",
    });
    await tearDownActiveCall(
      { update_muji_presence } as unknown as Parameters<
        typeof tearDownActiveCall
      >[0],
      "success",
    );
    expect(update_muji_presence).toHaveBeenCalledTimes(1);
    expect(update_muji_presence).toHaveBeenCalledWith(
      "chan@muc.test",
      "alice",
      false, // active
      false, // preparing
      false, // video
      { handRaised: false, muted: false }, // teardown clears all in-call state (#1030)
    );
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("tearDownActiveCall on a MUC call dispatches Muji session-terminate AND clears the presence", async () => {
    const send_muji_session_terminate = mock(async () => undefined);
    const events = wireClientEvents();
    const update_muji_presence = mockUpdateMujiPresenceWithEcho(events.emitPresence);
    $callState.set({
      phase: "active",
      peer: "chan@muc.test",
      sid: "muc-attempt",
      media: audioVideo,
      join,
      kind: "muc",
      selfNick: "alice",
    });
    await tearDownActiveCall(
      {
        send_muji_session_terminate,
        update_muji_presence,
      } as unknown as Parameters<typeof tearDownActiveCall>[0],
      "gone",
    );
    expect(send_muji_session_terminate).toHaveBeenCalledTimes(1);
    expect(send_muji_session_terminate).toHaveBeenCalledWith(
      "chan@muc.test",
      "muc-attempt",
    );
    expect(update_muji_presence).toHaveBeenCalledTimes(1);
    expect(update_muji_presence).toHaveBeenCalledWith(
      "chan@muc.test",
      "alice",
      false, // active
      false, // preparing
      false, // video
      { handRaised: false, muted: false }, // teardown clears all in-call state (#1030)
    );
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("tearDownActiveCall clears only this MUC resource's active owner", async () => {
    applyMucCallPresence({
      from: "chan@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/desktop",
      muji: { preparing: false, active: true },
    });
    applyMucCallPresence({
      from: "chan@muc.test/alice",
      presence_type: "available",
      muc_jid: "alice@waddle.test/mobile",
      muji: { preparing: false, active: true },
    });
    $callState.set({
      phase: "active",
      peer: "chan@muc.test",
      sid: "muc-attempt",
      media: audioVideo,
      join,
      kind: "muc",
      selfNick: "alice",
      selfFullJid: "alice@waddle.test/desktop",
    });
    const update_muji_presence = mock(async () => {});
    const send_muji_session_terminate = mock(async () => {});

    await tearDownActiveCall(
      {
        update_muji_presence,
        send_muji_session_terminate,
      } as unknown as Parameters<typeof tearDownActiveCall>[0],
      "success",
    );

    expect(update_muji_presence).toHaveBeenCalledTimes(1);
    expect(send_muji_session_terminate).toHaveBeenCalledTimes(1);
    expect($mucCallParticipants.get()["chan@muc.test"]).toEqual(["alice"]);
  });

  test("beginMucCall treats active Muji presence failure as fatal and rolls back SFU/session work", async () => {
    const events = wireClientEvents();
    const send_muji_session_terminate = mock(async () => undefined);
    const send_muji_session_initiate = mockSendMujiSessionInitiateWithAccept({
      url: "wss://livekit.test",
      room: "chan@muc.test",
      identity: "alice@waddle.test/web",
      token: "jwt.payload.sig",
    }, events.emitCall);
    const update_muji_presence = mock(
      async (
        roomJid: string,
        nick: string,
        active: boolean,
        preparing: boolean,
        _video: boolean,
      ) => {
        if (preparing) {
          events.emitPresence({
            from: `${roomJid}/${nick}`,
            presence_type: "available",
            muji: { preparing: true, active: false },
          });
          return;
        }
        if (active) throw new Error("presence publish failed");
        events.emitPresence({
          from: `${roomJid}/${nick}`,
          presence_type: "available",
        });
      },
    );
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    await expect(beginMucCall(
      {
        send_muji_session_initiate,
        send_muji_session_terminate,
        update_muji_presence,
      },
      "Chan@MUC.Test/ignored",
      audioVideo,
      "alice",
    )).rejects.toThrow("presence publish failed");
    expect(send_muji_session_initiate).not.toHaveBeenCalled();
    expect(send_muji_session_terminate).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
    expect($mucCallParticipants.get()["chan@muc.test"]).toBeUndefined();
  });

  test("room switch preserves the old room's active MUC call and keeps MUC membership", async () => {
    const events = wireClientEvents();
    const send_muji_session_terminate = mock(async () => undefined);
    const update_muji_presence = mockUpdateMujiPresenceWithEcho(events.emitPresence);
    const leave_room = mock(async () => undefined);
    const operationOrder: string[] = [];
    const xmpp = {
      send_muji_session_terminate: mock(async (roomJid: string, sid: string) => {
        operationOrder.push(`terminate:${roomJid}:${sid}`);
        await send_muji_session_terminate(roomJid, sid);
      }),
      update_muji_presence: mock(async (
        roomJid: string,
        nick: string,
        active: boolean,
        preparing: boolean,
        video: boolean,
      ) => {
        operationOrder.push(`presence:${active}:${preparing}`);
        await update_muji_presence(roomJid, nick, active, preparing, video);
      }),
      join_room: mock(async () => undefined),
      leave_room: mock(async (roomJid: string, nick: string) => {
        operationOrder.push(`leave:${roomJid}/${nick}`);
        await leave_room(roomJid, nick);
      }),
    };
    const client = events.client as unknown as {
      xmpp: typeof xmpp;
      currentRoom: string | null;
      joinedMucs: Map<string, Promise<void>>;
      performRoomSwitch: (roomJid: string) => Promise<void>;
    };
    Object.assign(client.xmpp, xmpp);
    client.currentRoom = "old@muc.test";
    client.joinedMucs.set("new@muc.test", Promise.resolve());
    $callState.set({
      phase: "active",
      peer: "old@muc.test",
      sid: "old-muc-attempt",
      media: audioVideo,
      join,
      kind: "muc",
      selfNick: "alice",
    });

    await client.performRoomSwitch("new@muc.test");

    expect(operationOrder).toEqual([]);
    expect(xmpp.leave_room).not.toHaveBeenCalled();
    expect($callState.get()).toMatchObject({
      phase: "active",
      peer: "old@muc.test",
      sid: "old-muc-attempt",
      kind: "muc",
    });
  });

  test("room switch bails out cleanly when a concurrent disconnect races the join", async () => {
    const events = wireClientEvents();
    let releaseJoin: (() => void) | null = null;
    let joinStarted: (() => void) | null = null;
    const joinGate = new Promise<void>((resolve) => {
      releaseJoin = resolve;
    });
    const joinStartedWait = new Promise<void>((resolve) => {
      joinStarted = resolve;
    });
    let joinCalls = 0;
    const join_room = mock(async () => {
      joinCalls += 1;
      if (joinCalls === 1) {
        joinStarted?.();
        await joinGate;
      }
    });
    const leave_room = mock(async () => undefined);
    const disconnect = mock(async () => undefined);
    const xmpp = { leave_room, join_room, disconnect };
    const client = events.client as unknown as {
      xmpp: typeof xmpp;
      currentRoom: string | null;
      performRoomSwitch: (roomJid: string) => Promise<void>;
      disconnect: () => Promise<void>;
    };
    Object.assign(client.xmpp, xmpp);
    client.currentRoom = "old@muc.test";

    const switched = client.performRoomSwitch("new@muc.test");
    await joinStartedWait;
    const disconnected = client.disconnect();
    releaseJoin?.();
    await switched;
    await disconnected;

    expect(join_room).toHaveBeenCalledTimes(1);
    expect(leave_room.mock.calls.map((call) => call[0] as string)).not.toContain("old@muc.test");
    expect(client.currentRoom).toBe(null);
  });

  test("room switch preserves the old room's pending MUC call without leaving the MUC", async () => {
    const events = wireClientEvents();
    const send_muji_session_initiate = mock(async () => undefined);
    const send_muji_session_terminate = mock(async () => undefined);
    const update_muji_presence = mockUpdateMujiPresenceWithEcho(events.emitPresence);
    const leave_room = mock(async () => undefined);
    const operationOrder: string[] = [];
    const xmpp = {
      send_muji_session_initiate: mock(async (roomJid: string, _sid: string, video: boolean) => {
        operationOrder.push(`initiate:${roomJid}:${video}`);
        await send_muji_session_initiate(roomJid, _sid, video);
      }),
      send_muji_session_terminate: mock(async (roomJid: string, sid: string) => {
        operationOrder.push(`terminate:${roomJid}:${sid}`);
        await send_muji_session_terminate(roomJid, sid);
      }),
      update_muji_presence: mock(async (
        roomJid: string,
        nick: string,
        active: boolean,
        preparing: boolean,
        video: boolean,
      ) => {
        operationOrder.push(`presence:${active}:${preparing}:${video}`);
        await update_muji_presence(roomJid, nick, active, preparing, video);
      }),
      join_room: mock(async () => undefined),
      leave_room: mock(async (roomJid: string, nick: string) => {
        operationOrder.push(`leave:${roomJid}/${nick}`);
        await leave_room(roomJid, nick);
      }),
    };
    const client = events.client as unknown as {
      xmpp: typeof xmpp;
      currentRoom: string | null;
      joinedMucs: Map<string, Promise<void>>;
      performRoomSwitch: (roomJid: string) => Promise<void>;
    };
    Object.assign(client.xmpp, xmpp);
    const wiredXmpp = client.xmpp;
    client.currentRoom = "old@muc.test";
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    const pending = beginMucCall(wiredXmpp, "old@muc.test", audioVideo, "alice");
    await flushCallSideEffects();
    await flushCallSideEffects();
    expect($callState.get().phase).toBe("muc-pending");
    expect(send_muji_session_initiate).toHaveBeenCalledTimes(1);
    const attemptSid = firstMockCallArg(send_muji_session_initiate, 1);
    expect(attemptSid).not.toBe("old@muc.test");
    client.joinedMucs.set("new@muc.test", Promise.resolve());

    await client.performRoomSwitch("new@muc.test");

    events.emitCall({
      kind: "session-accept",
      from: "calls.waddle.test",
      sid: attemptSid,
      media: audioVideo,
      join: { ...join, room: "old@muc.test" },
    });
    await expect(pending).resolves.toBeUndefined();
    expect(operationOrder).toEqual([
      "presence:false:true:false",
      "presence:true:false:true",
      "initiate:old@muc.test:true",
    ]);
    expect(xmpp.leave_room).not.toHaveBeenCalled();
    expect($callState.get()).toMatchObject({
      phase: "active",
      peer: "old@muc.test",
      sid: attemptSid,
      kind: "muc",
    });
  });

  test("room switch preserves a MUC start waiting for the old room's preparing echo", async () => {
    const events = wireClientEvents();
    const send_muji_session_initiate = mock(async () => undefined);
    const update_muji_presence = mock(async () => undefined);
    const leave_room = mock(async () => undefined);
    const operationOrder: string[] = [];
    const xmpp = {
      send_muji_session_initiate: mock(async (roomJid: string, _sid: string, video: boolean) => {
        operationOrder.push(`initiate:${roomJid}:${video}`);
        await send_muji_session_initiate(roomJid, _sid, video);
      }),
      update_muji_presence: mock(async (
        roomJid: string,
        nick: string,
        active: boolean,
        preparing: boolean,
        video: boolean,
      ) => {
        operationOrder.push(`presence:${roomJid}/${nick}:${active}:${preparing}:${video}`);
        await update_muji_presence(roomJid, nick, active, preparing, video);
      }),
      join_room: mock(async () => undefined),
      leave_room: mock(async (roomJid: string, nick: string) => {
        operationOrder.push(`leave:${roomJid}/${nick}`);
        await leave_room(roomJid, nick);
      }),
    };
    const client = events.client as unknown as {
      xmpp: typeof xmpp;
      currentRoom: string | null;
      joinedMucs: Map<string, Promise<void>>;
      performRoomSwitch: (roomJid: string) => Promise<void>;
    };
    Object.assign(client.xmpp, xmpp);
    const wiredXmpp = client.xmpp;
    client.currentRoom = "old@muc.test";
    const { beginMucCall } = await import("../src/lib/calls/call-store");
    const pending = beginMucCall(wiredXmpp, "old@muc.test", audioVideo, "alice");
    await flushCallSideEffects();
    await flushCallSideEffects();
    expect($callState.get().phase).toBe("muc-pending");
    expect(send_muji_session_initiate).toHaveBeenCalledTimes(0);
    client.joinedMucs.set("new@muc.test", Promise.resolve());

    await client.performRoomSwitch("new@muc.test");

    events.emitPresence({
      from: "old@muc.test/alice",
      presence_type: "available",
      muji: { preparing: true, active: false },
    });
    await flushCallSideEffects();

    expect(send_muji_session_initiate).toHaveBeenCalledTimes(1);
    const attemptSid = firstMockCallArg(send_muji_session_initiate, 1);
    events.emitCall({
      kind: "session-accept",
      from: "calls.waddle.test",
      sid: attemptSid,
      media: audioVideo,
      join: { ...join, room: "old@muc.test" },
    });
    await expect(pending).resolves.toBeUndefined();
    expect(operationOrder).toEqual([
      "presence:old@muc.test/alice:false:true:false",
      "presence:old@muc.test/alice:true:false:true",
      "initiate:old@muc.test:true",
    ]);
    expect(xmpp.leave_room).not.toHaveBeenCalled();
    expect($callState.get()).toMatchObject({
      phase: "active",
      peer: "old@muc.test",
      sid: attemptSid,
      kind: "muc",
    });
  });
});

describe("scheduleOutgoingTimeout", () => {
  test("fires retract and ends the call when the timer elapses on outgoing", async () => {
    const sender = mockSender();
    beginOutgoingCall("bob@waddle.test", "c-timeout", audioVideo);
    scheduleOutgoingTimeout(sender, "c-timeout", 10);
    // Wait past the timer.
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(sender.send_call_retract).toHaveBeenCalledTimes(1);
    expect(sender.send_call_retract).toHaveBeenCalledWith("bob@waddle.test", "c-timeout");
    expect($callState.get()).toEqual({
      phase: "ended",
      sid: "c-timeout",
      reason: "timeout",
    });
  });

  test("is a no-op if the call has already transitioned out of outgoing", async () => {
    const sender = mockSender();
    beginOutgoingCall("bob@waddle.test", "c-fast", audioVideo);
    scheduleOutgoingTimeout(sender, "c-fast", 10);
    // Simulate a fast peer answering — call moves to active before timer fires.
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c-fast",
      media: audioVideo,
      join,
    });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(sender.send_call_retract).not.toHaveBeenCalled();
  });

  test("is a no-op if the sid no longer matches", async () => {
    const sender = mockSender();
    beginOutgoingCall("bob@waddle.test", "c-old", audioVideo);
    scheduleOutgoingTimeout(sender, "c-old", 10);
    // Replace with a new outgoing call (different sid) — old timer
    // should not fire retract on the new call.
    beginOutgoingCall("carol@waddle.test", "c-new", audioVideo);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(sender.send_call_retract).not.toHaveBeenCalled();
  });
});
