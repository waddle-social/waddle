import { describe, expect, mock, test } from "bun:test";
import { BrowserXmppClient } from "../src/lib/xmpp/client";
import type { WaddleSession } from "../src/lib/server-auth";

function session(partial: Partial<WaddleSession> = {}): WaddleSession {
  return {
    session_id: "s1",
    user_id: "u1",
    username: "alice",
    avatar_url: null,
    xmpp_localpart: "alice",
    jid: "alice@example.com",
    xmpp_websocket_url: "wss://example.com/xmpp",
    is_expired: false,
    expires_at: null,
    ...partial,
  };
}

function connectedClient(xmpp: Record<string, unknown>): BrowserXmppClient {
  const client = new BrowserXmppClient(session());
  (client as unknown as { xmpp: unknown }).xmpp = xmpp;
  (client as unknown as { connected: boolean }).connected = true;
  return client;
}

describe("BrowserXmppClient MDS publish preflight", () => {
  test("does not publish when the server lacks pubsub publish-options", async () => {
    const publish = mock(async () => undefined);
    const supports = mock(async () => false);
    const client = connectedClient({
      publish_mds_displayed: publish,
      supports_mds_publish_options: supports,
    });

    await client.publishMdsDisplayed("bob@example.com", "sid-1", "example.com");

    expect(supports).toHaveBeenCalledTimes(1);
    expect(publish).not.toHaveBeenCalled();
  });

  test("publishes when publish-options are advertised and caches the preflight", async () => {
    const publish = mock(async () => undefined);
    const supports = mock(async () => true);
    const client = connectedClient({
      publish_mds_displayed: publish,
      supports_mds_publish_options: supports,
    });

    await client.publishMdsDisplayed("bob@example.com", "sid-1", "example.com");
    await client.publishMdsDisplayed("bob@example.com", "sid-2", "example.com");

    expect(supports).toHaveBeenCalledTimes(1);
    expect(publish).toHaveBeenCalledTimes(2);
  });
});
