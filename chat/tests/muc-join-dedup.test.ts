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
  fullJid: string;
};

/** A mock WASM handle that records join_room calls and lets the test
 *  deliver the room self-presence that resolves a join. */
function connectedClient() {
  const client = new BrowserXmppClient(session());
  const state = client as unknown as PrivateState;
  let onPresence: PresenceCb | null = null;
  const joinRoom = mock(async () => undefined);
  const xmpp = {
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
