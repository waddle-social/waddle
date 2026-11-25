/**
 * XEP-0280 carbons enable: the web sends none of its own.
 *
 * The shared core asks for carbons once after every fresh bind, and not
 * after a XEP-0198 resume, where the server keeps the setting
 * (`finish_binding` in waddle-xmpp-client, with its own tests). The web
 * used to ask as well, at one time from two places (#754: two IQs per
 * connect). Contract here: this layer puts no carbons enable on the wire,
 * on a fresh session or a resumed one.
 *
 * These tests poke private state on `BrowserXmppClient` directly — see
 * the comment block in `resume-ordering.test.ts` for the rationale.
 */
import { describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

type PrivateState = {
  wireEvents: (xmpp: unknown) => void;
};

function makeWiredClient() {
  const client = new BrowserXmppClient(session());
  const state = client as unknown as PrivateState;
  const callbacks: {
    onConnected?: () => void;
    onSessionLifecycle?: (event: string) => void;
  } = {};
  const enableCarbons = mock(async () => {});
  const sendRawIq = mock(async (_xml: string) => undefined);
  const xmpp = {
    enableCarbons,
    send_raw_iq: sendRawIq,
    set_on_connected: (cb: () => void) => {
      callbacks.onConnected = cb;
    },
    set_on_session_lifecycle: (cb: (event: string) => void) => {
      callbacks.onSessionLifecycle = cb;
    },
  };
  state.wireEvents(xmpp);
  return { client, callbacks, enableCarbons, sendRawIq };
}

async function flushAsync(times = 4): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await new Promise((r) => setTimeout(r, 0));
  }
}

/** The carbons enables among the raw IQs the web layer sent. */
function carbonsEnables(sendRawIq: { mock: { calls: unknown[][] } }): number {
  return sendRawIq.mock.calls.filter(([xml]) => String(xml).includes("urn:xmpp:carbons:2")).length;
}

describe("XEP-0280 carbons enable is the shared core's (#754)", () => {
  // A fresh session and a XEP-0198 resume, as the WASM core names them.
  test.each(["fresh", "resumed"])("a %s session sends no carbons enable from the web layer", async (lifecycle) => {
    const { callbacks, enableCarbons, sendRawIq } = makeWiredClient();

    // Real connect order: transport up first, then session-ready.
    callbacks.onConnected?.();
    callbacks.onSessionLifecycle?.(lifecycle);
    await flushAsync();

    expect(enableCarbons.mock.calls).toHaveLength(0);
    expect(carbonsEnables(sendRawIq)).toBe(0);
  });
});
