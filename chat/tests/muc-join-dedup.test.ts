/**
 * #1221 — reconnect MUC-join dedup.
 *
 * A prod server restart made 5 clients fan out 96 MUC joins in ~2s with
 * heavy per-room duplication. Two client-side amplifiers:
 *
 *   * Case-key mismatch: retained room JIDs were lowercased while
 *     autojoin/`joinedMucs` kept raw bookmark case, so the same room
 *     joined under two keys. Every join tracker must key on the single
 *     canonical `roomJoinKey` (lowercased bare JID); the raw JID still
 *     goes on the wire.
 *   * No per-epoch single-flight: a failed auto-join was retried on the
 *     next trigger (15s self-presence-timeout retry amplifier), and the
 *     three fan-out triggers per session each re-attempted every room.
 *     `fanOutAutoJoin` must attempt each room key at most once per epoch.
 *
 * These tests poke private state on `BrowserXmppClient` directly — see
 * the comment block in `resume-ordering.test.ts` for the rationale.
 */
import { afterEach, describe, expect, mock, test } from "bun:test";
import type { WaddleSession } from "../src/lib/server-auth";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime/memory-durable-store";
import { noopWasmClientCallbacks } from "./helpers/wasm-client-callbacks";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com/desktop",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

type PresenceCb = (presence: {
  from?: string;
  presence_type?: string;
  muc_jid?: string;
}) => void;

type PrivateState = {
  xmpp: unknown;
  connected: boolean;
  wireEvents: (xmpp: unknown) => void;
  ensureJoined: (roomJid: string) => Promise<void>;
  fanOutAutoJoin: (roomJids: ReadonlyArray<string>, concurrency?: number) => Promise<void>;
  joinedMucReady: Set<string>;
  autoJoinAttemptedRoomKeys: Set<string>;
  fullJid: string;
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

/** A mock WASM handle that records join_room calls and lets the test
 *  deliver the room self-presence that resolves a join. */
function connectedClient() {
  const client = new BrowserXmppClient(session(), {
    durableRuntimeStore: new MemoryDurableOutboundStore(),
  });
  createdClients.push(client);
  const state = client as unknown as PrivateState;
  let onPresence: PresenceCb | null = null;
  const joinRoom = mock(async () => undefined);
  const xmpp = {
    ...noopWasmClientCallbacks(),
    join_room: joinRoom,
    set_on_presence(cb: PresenceCb) {
      onPresence = cb;
    },
  };
  state.xmpp = xmpp;
  state.connected = true;
  state.wireEvents(xmpp);
  const deliverSelfPresence = (roomBareJid: string) => {
    onPresence?.({
      from: `${roomBareJid}/alice`,
      presence_type: "available",
      muc_jid: state.fullJid,
    });
  };
  return { client, state, joinRoom, deliverSelfPresence };
}

describe("#1221 canonical join key coalesces case variants", () => {
  test("a second join for a case-variant of a joined room sends no new presence", async () => {
    const { state, joinRoom, deliverSelfPresence } = connectedClient();
    const upper = "Channel1@conference.example.com";
    const lower = "channel1@conference.example.com";

    const first = state.ensureJoined(upper);
    deliverSelfPresence(lower); // server echoes the canonical (lowercased) room
    await first;

    await state.ensureJoined(lower);

    expect(joinRoom).toHaveBeenCalledTimes(1);
    expect(state.joinedMucReady.has(lower)).toBe(true);
  });
});

describe("#1221 fanOutAutoJoin is single-flight per epoch", () => {
  test("a failed auto-join is not retried by a later trigger in the same epoch", async () => {
    const client = new BrowserXmppClient(session(), {
      durableRuntimeStore: new MemoryDurableOutboundStore(),
    });
    createdClients.push(client);
    const state = client as unknown as PrivateState;
    const room = "room2@conference.example.com";
    const joinRoom = mock(async () => {
      throw new Error("join rejected");
    });
    const xmpp = {
      ...noopWasmClientCallbacks(),
      join_room: joinRoom,
    };
    state.xmpp = xmpp;
    state.connected = true;
    state.wireEvents(xmpp);

    await expect(state.fanOutAutoJoin([room])).rejects.toThrow(
      "One or more MUC auto-joins failed",
    ); // first trigger: join_room throws
    await state.fanOutAutoJoin([room]); // second trigger, same epoch: must not retry

    expect(joinRoom).toHaveBeenCalledTimes(1);
    expect(state.autoJoinAttemptedRoomKeys.has(room)).toBe(true);
  });

  test("concurrent fan-out triggers for the same room send a single join", async () => {
    const { state, joinRoom, deliverSelfPresence } = connectedClient();
    const room = "room3@conference.example.com";

    const a = state.fanOutAutoJoin([room]);
    const b = state.fanOutAutoJoin([room]);
    deliverSelfPresence(room);
    await Promise.all([a, b]);

    expect(joinRoom).toHaveBeenCalledTimes(1);
  });

  test("case variants in the fan-out input list collapse to one join", async () => {
    const { state, joinRoom, deliverSelfPresence } = connectedClient();

    const fan = state.fanOutAutoJoin([
      "Room4@conference.example.com",
      "room4@conference.example.com",
    ]);
    deliverSelfPresence("room4@conference.example.com");
    await fan;

    expect(joinRoom).toHaveBeenCalledTimes(1);
  });
});
