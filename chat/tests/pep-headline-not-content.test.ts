import { describe, expect, test } from "bun:test";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { WasmMessage } from "../src/lib/xmpp/wasm-types";
import { dmMessageFromArchived, roomMessageFromArchived } from "../src/lib/xmpp/wasm-message-codecs";
import { installMockBrowserGlobals } from "./helpers/mock-browser-storage";
import { nullResumePersistence } from "../src/lib/xmpp/resume-persistence";

installMockBrowserGlobals();
const session = { username: "alice", jid: "alice@example.com", session_id: "test", xmpp_websocket_url: "wss://example.com/ws" } as WaddleSession;

/**
 * XEP-0163 PEP notifications (avatar metadata, mood, activity, …) arrive
 * as `<message type='headline'>` carrying only a pubsub#event child. The
 * wasm core surfaces them as body-less messages; they must never become
 * timeline rows, DM conversations, unread counts or room activity.
 */
function pepHeadline(from: string, overrides: Partial<WasmMessage> = {}): WasmMessage {
  return {
    id: `pep-${from}`,
    from,
    to: session.jid,
    message_type: "headline",
    is_muc: false,
    reaction_emojis: [],
    markup_spans: [],
    mention_uris: [],
    references: [],
    is_sticker: false,
    shared_files: [],
    link_previews: [],
    ...overrides,
  };
}

function harness() {
  const client = new BrowserXmppClient(session, nullResumePersistence);
  let inbound!: (message: WasmMessage) => void;
  const xmpp = { set_on_message(callback: typeof inbound) { inbound = callback; } };
  const internal = client as unknown as { xmpp: typeof xmpp; connected: boolean; wireEvents: (host: typeof xmpp) => void };
  internal.xmpp = xmpp;
  internal.connected = true;
  internal.wireEvents(xmpp);
  const effects: string[] = [];
  client.setDirectMessageHandler(() => effects.push("directMessage"));
  client.setMessageHandler(() => effects.push("message"));
  client.setActivityHandler(() => effects.push("activity"));
  client.setInboxPushHandler(() => effects.push("inboxPush"));
  return { client, effects, inbound: (message: WasmMessage) => inbound(message) };
}

describe("body-less PEP headline notifications", () => {
  test.each([
    ["a peer's avatar metadata", "bob@example.com"],
    ["our own mood from another resource", "alice@example.com"],
    ["a pubsub service", "pubsub.example.com"],
  ])("%s creates no row, conversation, unread or activity", async (_label, from) => {
    const h = harness();
    h.inbound(pepHeadline(from));
    h.inbound(pepHeadline(from, { thread: "t-1" }));
    expect(h.effects).toEqual([]);
    await h.client.disconnect();
  });

  test("control: the same harness does surface a chat message with a body", async () => {
    const h = harness();
    h.inbound(pepHeadline("bob@example.com", { message_type: "chat", body: "hi" }));
    expect(h.effects).toEqual(["directMessage"]);
    await h.client.disconnect();
  });

  test("the codecs refuse body-less headlines on the archive and live paths", () => {
    const message = { ...pepHeadline("bob@example.com"), mam_id: "m-1" };
    expect(dmMessageFromArchived(message, session.jid, "live")).toBeNull();
    expect(roomMessageFromArchived({ ...message, is_muc: true, from: "room@muc.example.com" }, "live")).toBeNull();
  });
});
