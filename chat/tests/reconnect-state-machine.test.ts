/**
 * #1164: honest reconnect state machine.
 *
 * The client must distinguish recoverable drops (keep retrying, show
 * "reconnecting") from terminal failures (auth failure / resource
 * conflict / retry exhaustion — emit `state: "error"` and stop), and
 * a stalled connect attempt must tear down and reschedule instead of
 * silently rejecting.
 *
 * These tests drive `wireEvents` with a stub WASM handle (the same
 * harness pattern as `xmpp-telemetry.test.ts`) and observe the
 * user-facing state through the client's status handler.
 */
import { describe, expect, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { XmppStatusSnapshot } from "../src/lib/xmpp/types";

type StreamErrorPayload = { detail?: string | null; condition?: string | null };

type StubXmpp = {
  set_on_error: (cb: (payload: StreamErrorPayload) => void) => void;
  set_on_disconnected: (cb: () => void) => void;
  get_resume_state: () => null;
};

type PrivateState = {
  xmpp: unknown;
  connected: boolean;
  wireEvents: (xmpp: StubXmpp) => void;
  reconnect: { clearTimer: () => void };
};

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

function createHarness() {
  const client = new BrowserXmppClient(session());
  const state = client as unknown as PrivateState;
  const statuses: XmppStatusSnapshot[] = [];
  const scheduled: Array<{ attempt: number; delayMs: number }> = [];
  client.setStatusHandler((snapshot) => statuses.push(snapshot));
  client.onReconnectScheduled((info) => scheduled.push(info));

  let fireError: (payload: StreamErrorPayload) => void = () => {};
  let fireDisconnected: () => void = () => {};
  const stub: StubXmpp = {
    set_on_error(cb) { fireError = cb; },
    set_on_disconnected(cb) { fireDisconnected = cb; },
    get_resume_state: () => null,
  };
  state.xmpp = stub;
  state.connected = true;
  state.wireEvents(stub);

  return {
    client,
    state,
    stub,
    statuses,
    scheduled,
    fireError: (payload: StreamErrorPayload) => fireError(payload),
    fireDisconnected: () => fireDisconnected(),
  };
}

describe("terminal error state (#1164 slice 1)", () => {
  test("a not-authorized stream error reaches state \"error\" and never schedules a retry", () => {
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({ condition: "not-authorized", detail: "SASL authentication failed" });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("error");
    expect(scheduled).toHaveLength(0);
    state.reconnect.clearTimer();
  });

  test("a resource conflict is terminal too", () => {
    const { statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({ condition: "conflict", detail: "replaced by new connection" });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("error");
    expect(scheduled).toHaveLength(0);
  });

  test("a recoverable stream error keeps the reconnecting loop", () => {
    const { state, statuses, scheduled, fireError, fireDisconnected } = createHarness();

    fireError({ condition: "system-shutdown", detail: "going down" });
    fireDisconnected();

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    state.reconnect.clearTimer();
  });
});

describe("retry-loop cap (#1164 slice 2)", () => {
  test("exhausting the reconnect attempts flips the client to terminal error", () => {
    const { state, stub, statuses, scheduled, fireDisconnected } = createHarness();

    for (let i = 0; i < 11; i += 1) {
      // Re-arm the handle + drop the pending timer so each disconnect
      // is accepted and each schedule() call actually runs.
      state.reconnect.clearTimer();
      state.xmpp = stub;
      state.connected = true;
      fireDisconnected();
    }

    // 10 scheduled attempts, then exhaustion: no 11th schedule.
    expect(scheduled).toHaveLength(10);
    expect(statuses.at(-1)?.state).toBe("error");
    state.reconnect.clearTimer();
  });
});

describe("room-scoped queueing keeps the global banner honest (#1164 slice 4)", () => {
  test("sendGroupMessage to an unjoined room while connected leaves the status untouched", async () => {
    const { client, statuses } = createHarness();

    const result = await client.sendGroupMessage("space-1", "general", "hello");

    expect(result?.state).toBe("queued");
    expect(statuses).toHaveLength(0);
  });
});

describe("connect-timeout teardown (#1164 slice 3)", () => {
  test("a stalled connect tears down the handle, emits reconnecting, and reschedules", async () => {
    const client = new BrowserXmppClient(session());
    const state = client as unknown as PrivateState & {
      connectTimeoutMs: number;
      doConnect: () => Promise<void>;
    };
    const statuses: XmppStatusSnapshot[] = [];
    const scheduled: Array<{ attempt: number; delayMs: number }> = [];
    client.setStatusHandler((snapshot) => statuses.push(snapshot));
    client.onReconnectScheduled((info) => scheduled.push(info));

    let stalledHandleDisconnects = 0;
    state.connectTimeoutMs = 10;
    state.doConnect = async () => {
      // Socket opened but the session never establishes.
      state.xmpp = { disconnect: async () => { stalledHandleDisconnects += 1; } };
    };

    await expect(client.connect()).rejects.toThrow("Reconnection timed out");

    expect(statuses.at(-1)?.state).toBe("reconnecting");
    expect(scheduled).toHaveLength(1);
    expect(state.xmpp).toBeNull();
    expect(stalledHandleDisconnects).toBe(1);
    state.reconnect.clearTimer();
  });
});
