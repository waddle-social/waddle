import { describe, expect, test } from "bun:test";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";
import type { WasmMessage, WasmMessageRejection } from "../src/lib/xmpp/wasm-types";
import { dmMessageFromArchived, roomMessageFromArchived } from "../src/lib/xmpp/wasm-message-codecs";
import { listQueuedDmMessages } from "../src/lib/outbound-queue-store";
import { installMockBrowserGlobals } from "./helpers/mock-browser-storage";
import { nullResumePersistence } from "../src/lib/xmpp/resume-persistence";
import { ref } from "vue";
import { useDmLiveMerge } from "../src/dms/live-merge";
import { useChatSend } from "../src/dms/chat-send";
import type { TimelineMessage } from "../src/lib/chat-ui";

installMockBrowserGlobals();
const session: WaddleSession = { username: "alice", jid: "alice@example.com", session_id: "test", xmpp_websocket_url: "wss://example.com/ws" };

function harness() {
  const client = new BrowserXmppClient(session, nullResumePersistence);
  let rejected!: (rejection: WasmMessageRejection) => void;
  let acked!: (id: string) => void;
  let inbound!: (message: WasmMessage) => void;
  const xmpp = {
    set_on_message_rejected(callback: typeof rejected) { rejected = callback; },
    set_on_message_delivery_acked(callback: typeof acked) { acked = callback; },
    set_on_message(callback: typeof inbound) { inbound = callback; },
    send_chat_message: async (_peer: string, _body: string, opts: { stanza_id: string }) => opts.stanza_id,
  };
  const internal = client as unknown as { xmpp: typeof xmpp; connected: boolean; wireEvents: (host: typeof xmpp) => void };
  internal.xmpp = xmpp;
  internal.connected = true;
  internal.wireEvents(xmpp);
  const messages = ref<TimelineMessage[]>([]);
  const send = useChatSend({
    session: ref(session), xmppClient: ref(client), activePeerJid: ref("chat@example.com"), messages,
    draft: ref("hello"), actionError: ref(""), clearActionError: () => {}, normalizeError: String,
    scrollToPinnedEdgeAndPin: async () => true, onSendComplete: () => {},
  });
  client.setMessageAckHandler(send.onMessageAck);
  client.setMessageDeliveryFailureHandler(send.onMessageDeliveryFailure);
  return { client, xmpp, messages, send, rejected: (event: WasmMessageRejection) => rejected(event), acked: (id: string) => acked(id), inbound: (message: WasmMessage) => inbound(message) };
}

function rejection(id: string): WasmMessageRejection {
  return { stanza_id: id, from: "chat@example.com", to: "alice@example.com", error: { error_type: "cancel", condition: "service-unavailable" } };
}

describe("message rejection across the browser and timeline", () => {
  test.each(["before-result", "before-ack", "after-ack"])("shows failed with rejection %s and no incoming content", async (order) => {
    const h = harness();
    const incoming: unknown[] = [];
    h.client.setDirectMessageHandler((message) => incoming.push(message));
    if (order === "before-result") h.xmpp.send_chat_message = async (_peer, _body, opts) => {
      h.rejected(rejection(opts.stanza_id));
      return opts.stanza_id;
    };
    await h.send.sendMessage();
    const id = h.messages.value[0]!.id;
    if (order === "after-ack") h.acked(id);
    if (order !== "before-result") h.rejected(rejection(id));
    h.acked(id);
    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.deliveryStatus).toBe("rejected");
    expect(h.messages.value[0]?.isSelf).toBe(true);
    expect(incoming).toEqual([]);
    expect(listQueuedDmMessages(session.jid, "chat@example.com", "account")).toEqual([]);
    await h.client.disconnect();
  });

  test("error payloads never run incoming message effects or archive conversion", async () => {
    const h = harness();
    const incoming: unknown[] = [];
    h.client.setDirectMessageHandler((message) => incoming.push(message));
    const error = { id: "m", from: "chat@example.com", to: session.jid, message_type: "error", body: "echoed body", is_muc: false } as WasmMessage;
    h.inbound(error);
    h.inbound({ ...error, carbon: { received: true, sent: false } });
    expect(incoming).toEqual([]);
    expect(dmMessageFromArchived({ ...error, mam_id: "archive-error" }, session.jid)).toBeNull();
    expect(roomMessageFromArchived({ ...error, mam_id: "archive-error", is_muc: true })).toBeNull();
    await h.client.disconnect();
  });

  test("a verified sent carbon supplies identity for a rejection on another device", async () => {
    const h = harness();
    const failures: Array<[string, string | undefined]> = [];
    h.client.setMessageDeliveryFailureHandler((id, reason) => failures.push([id, reason]));
    const sent = { id: "other-device", from: `${session.jid}/phone`, to: "chat@example.com", message_type: "chat", body: "hello", is_muc: false, carbon: { sent: true, received: false }, reaction_emojis: [], markup_spans: [], mention_uris: [], references: [], shared_files: [] } as WasmMessage;
    h.inbound(sent);
    h.rejected(rejection("other-device"));
    expect(failures).toEqual([["other-device", "rejected"]]);
    await h.client.disconnect();
  });

  test("a rejection reaches the sent-carbon row after the resume buffer drains", async () => {
    const h = harness();
    const merge = useDmLiveMerge({ session: ref(session), messages: h.messages,
      activePeerJid: ref("chat@example.com"), pendingEchoClientIds: h.send.pendingEchoClientIds,
      scrollToPinnedEdgeAndPin: async () => true, persistLastSeen: () => {}, isFeedVisible: () => true });
    h.client.setDirectMessageHandler(merge.handleIncomingMessage);
    const internal = h.client as unknown as {
      openResumeBarrier: (host: typeof h.xmpp, promise: Promise<void>) => void;
      completeResumeBarrier: (host: typeof h.xmpp) => void;
    };
    internal.openResumeBarrier(h.xmpp, Promise.resolve());
    h.inbound({ id: "buffered", from: `${session.jid}/phone`, to: "chat@example.com", message_type: "chat", body: "hello", is_muc: false, carbon: { sent: true, received: false }, reaction_emojis: [], markup_spans: [], mention_uris: [], references: [], shared_files: [] } as WasmMessage);
    expect(h.messages.value).toEqual([]);
    h.rejected(rejection("buffered"));
    internal.completeResumeBarrier(h.xmpp);
    expect(h.messages.value).toHaveLength(1);
    expect(h.messages.value[0]?.deliveryStatus).toBe("rejected");
    await h.client.disconnect();
  });

  test("callbacks from an old connection cannot reject the current send", async () => {
    const h = harness();
    await h.send.sendMessage();
    const id = h.messages.value[0]!.id;
    (h.client as unknown as { xmpp: object }).xmpp = {};
    h.rejected(rejection(id));
    expect(h.messages.value[0]?.deliveryStatus).toBe("sending");
    await h.client.disconnect();
  });
});
