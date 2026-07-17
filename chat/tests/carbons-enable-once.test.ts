/**
 * #754 — duplicate XEP-0280 carbons enable.
 *
 * `enableCarbons` used to fire from two places on every connect: the WASM
 * `on_connected` hook AND the fresh branch of `runSessionReady`. One fresh
 * connect therefore put two `<enable xmlns='urn:xmpp:carbons:2'/>` IQs on
 * the wire, and a XEP-0198 resume (which preserves carbon state
 * server-side) re-enabled for no reason. Contract: exactly one enable per
 * fresh session, zero on resume.
 *
 * These tests poke private state on `BrowserXmppClient` directly — see
 * the comment block in `resume-ordering.test.ts` for the rationale.
 */
import { afterEach, describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime-durable-store";
import { noopWasmClientCallbacks } from "./helpers/wasm-client-callbacks";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

type PrivateState = {
  xmpp: unknown;
  connected: boolean;
  connectEpoch: number;
  outboundQueueHydration: Promise<void>;
  outboundQueue: {
    beginConnectionGeneration: (generation: number) => number;
  };
  wireEvents: (xmpp: unknown) => void;
};

const createdClients: BrowserXmppClient[] = [];

afterEach(async () => {
  for (const client of createdClients) {
    const state = client as unknown as { xmpp: null; connected: boolean };
    state.xmpp = null;
    state.connected = false;
  }
  await Promise.all(createdClients.map((client) => client.dispose()));
  createdClients.length = 0;
});

async function makeWiredClient() {
  const client = new BrowserXmppClient(session(), {
    durableRuntimeStore: new MemoryDurableOutboundStore(),
  });
  createdClients.push(client);
  const state = client as unknown as PrivateState;
  await state.outboundQueueHydration;
  state.outboundQueue.beginConnectionGeneration(state.connectEpoch);
  const callbacks: {
    onConnected?: () => void;
    onSessionLifecycle?: (event: "fresh" | "resumed") => void;
  } = {};
  const enableCarbons = mock(async () => {});
  const xmpp = {
    ...noopWasmClientCallbacks(),
    enableCarbons,
    set_on_connected: (cb: () => void) => {
      callbacks.onConnected = cb;
    },
    set_on_session_lifecycle: (cb: (event: "fresh" | "resumed") => void) => {
      callbacks.onSessionLifecycle = cb;
    },
  };
  state.xmpp = xmpp;
  state.connected = true;
  state.wireEvents(xmpp);
  return { client, callbacks, enableCarbons };
}

async function flushAsync(times = 4): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await new Promise((r) => setTimeout(r, 0));
  }
}

describe("XEP-0280 carbons enable cadence (#754)", () => {
  test("a fresh connect enables carbons exactly once", async () => {
    const { callbacks, enableCarbons } = await makeWiredClient();

    // Real connect order: transport up first, then session-ready.
    callbacks.onConnected?.();
    callbacks.onSessionLifecycle?.("fresh");
    await flushAsync();

    expect(enableCarbons.mock.calls.length).toBe(1);
  });

  test("a XEP-0198 resume does not re-enable carbons", async () => {
    const { callbacks, enableCarbons } = await makeWiredClient();

    callbacks.onConnected?.();
    callbacks.onSessionLifecycle?.("resumed");
    await flushAsync();

    expect(enableCarbons.mock.calls.length).toBe(0);
  });
});
