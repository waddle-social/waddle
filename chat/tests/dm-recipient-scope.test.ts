import { describe, expect, test } from "bun:test";
import { effectScope, ref, shallowRef } from "vue";
import { BrowserXmppClient, type LiveDmMessage } from "../src/lib/xmpp-client";
import { useDirectMessageConversations } from "../src/dms/conversations";
import type { WaddleSession } from "../src/lib/server-auth";
import type { WasmMessage } from "../src/lib/xmpp/wasm-types";
import { nullResumePersistence } from "../src/lib/xmpp/resume-persistence";
import { createStorageMock, installMockBrowserGlobals } from "./helpers/mock-browser-storage";

installMockBrowserGlobals({ beforeEachExtra: () => { Object.defineProperty(window, "sessionStorage", { value: createStorageMock(), configurable: true }); } });

describe("explicit occupant conversation context", () => {
  test("replies retain the selected full address without room discovery", async () => {
    const session: WaddleSession = { username: "alice", jid: "alice@example.com", session_id: "test", xmpp_websocket_url: "wss://example.com/ws" };
    const client = new BrowserXmppClient(session, nullResumePersistence);
    const scope = effectScope();
    const conversations = scope.run(() => useDirectMessageConversations(ref(session), shallowRef(client), ref([])))!;
    const peer = "room@nondefault-muc.service/Nick/Device";
    try {
      expect(client.isMucPmPeer(peer)).toBe(false);
      await conversations.openDm(peer, "muc-occupant");
      expect(conversations.activePeerJid.value).toBe(peer);
      expect(client.isMucPmPeer(peer)).toBe(true);
      expect(client.isMucPmPeer("room@nondefault-muc.service/Other")).toBe(false);
      const received: LiveDmMessage[] = [];
      client.setDirectMessageHandler((message) => { received.push(message); conversations.receiveIncomingDm(message); });
      const inbound = client as unknown as { handleMessage(message: WasmMessage): void };
      const message = { id: "reply", from: peer, to: session.jid, message_type: "chat", body: "private reply", muc_pm: true, is_muc: false, reaction_emojis: [], markup_spans: [], mention_uris: [], references: [], shared_files: [] } as WasmMessage;
      inbound.handleMessage(message);
      inbound.handleMessage({ ...message, id: "unselected", from: "room@nondefault-muc.service/Other" });
      expect(received).toHaveLength(1);
      expect(received[0]?.peerJid).toBe(peer);
      expect(received[0]?.mucPm).toBe(true);
      expect(conversations.conversations.value.map((conversation) => conversation.peerJid)).toEqual([peer]);
    } finally {
      scope.stop();
      await client.disconnect();
    }
  });
});
