import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import {
  clearDmCallActivities,
  readDmCallActivity,
} from "../src/lib/calls/dm-call-activity";
import type { WaddleSession } from "../src/lib/server-auth";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com",
    session_id: "tok",
    xmpp_websocket_url: "wss://example.com/ws",
  } as WaddleSession;
}

describe("DM call activity hydration", () => {
  beforeEach(() => {
    clearDmCallActivities();
  });

  afterEach(() => {
    clearDmCallActivities();
  });

  test("hydrates active call state from personal MAM pages without loading the DM timeline", async () => {
    const now = Date.now();
    const timestamp = (offsetMs: number) => new Date(now + offsetMs).toISOString();
    const latestPage = {
      messages: [
        {
          mam_id: "mam-proceed",
          from: "alice@example.com/web",
          to: "bob@example.com/phone",
          timestamp: timestamp(-60_000),
          call_event: {
            kind: "proceed",
            from: "alice@example.com/web",
            to: "bob@example.com/phone",
            sid: "call-1",
          },
        },
      ],
      first_id: "mam-proceed",
      last_id: "mam-proceed",
      is_complete: false,
    };
    const olderPage = {
      messages: [
        {
          mam_id: "mam-propose",
          from: "bob@example.com/phone",
          to: "alice@example.com/web",
          timestamp: timestamp(-300_000),
          call_event: {
            kind: "propose",
            from: "bob@example.com/phone",
            sid: "call-1",
            media: { audio: true, video: true },
          },
        },
      ],
      first_id: "mam-propose",
      last_id: "mam-propose",
      is_complete: true,
    };
    const fetchPersonalHistoryPage = mock(async (_max: number, pageParam: unknown) => {
      return (pageParam as { type: string }).type === "latest" ? latestPage : olderPage;
    });
    const client = new BrowserXmppClient(session());
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { xmpp: { fetch_personal_history_page: typeof fetchPersonalHistoryPage } }).xmpp = {
      fetch_personal_history_page: fetchPersonalHistoryPage,
    };

    const since = timestamp(-60 * 60 * 1000);
    await client.hydrateRecentDmCallActivities({
      since,
      maxPages: 2,
    });

    expect(fetchPersonalHistoryPage).toHaveBeenNthCalledWith(1, 100, { type: "latest", start: since });
    expect(fetchPersonalHistoryPage).toHaveBeenNthCalledWith(2, 100, { type: "before", before: "mam-proceed", start: since });
    expect(readDmCallActivity("bob@example.com")).toMatchObject({
      peerJid: "bob@example.com",
      sid: "call-1",
      state: "accepted",
      direction: "incoming",
      media: { audio: true, video: true },
    });
  });

  test("does not apply hydrated pages after the connected XMPP session changes", async () => {
    const now = Date.now();
    const timestamp = new Date(now - 60_000).toISOString();
    const page = {
      messages: [{
        mam_id: "mam-propose",
        from: "bob@example.com/phone",
        to: "alice@example.com/web",
        timestamp,
        call_event: {
          kind: "propose",
          from: "bob@example.com/phone",
          sid: "stale-call",
          media: { audio: true, video: false },
        },
      }],
      first_id: "mam-propose",
      last_id: "mam-propose",
      is_complete: true,
    };
    let resolveFetch: ((value: typeof page) => void) | null = null;
    const fetchPersonalHistoryPage = mock(() => new Promise<typeof page>((resolve) => {
      resolveFetch = resolve;
    }));
    const client = new BrowserXmppClient(session());
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { xmpp: { fetch_personal_history_page: typeof fetchPersonalHistoryPage } }).xmpp = {
      fetch_personal_history_page: fetchPersonalHistoryPage,
    };

    const since = new Date(now - 60 * 60 * 1000).toISOString();
    const hydration = client.hydrateRecentDmCallActivities({
      since,
      maxPages: 1,
    });
    for (let i = 0; i < 5; i += 1) await Promise.resolve();
    expect(fetchPersonalHistoryPage).toHaveBeenCalledTimes(1);

    (client as unknown as { xmpp: { fetch_personal_history_page: typeof fetchPersonalHistoryPage } }).xmpp = {
      fetch_personal_history_page: mock(async () => page),
    };
    resolveFetch?.(page);
    await hydration;

    expect(readDmCallActivity("bob@example.com")).toBeNull();
  });

  test("does not apply hydrated pages after disconnect", async () => {
    const now = Date.now();
    const since = new Date(now - 60 * 60 * 1000).toISOString();
    const page = {
      messages: [{
        mam_id: "mam-propose",
        from: "bob@example.com/phone",
        to: "alice@example.com/web",
        timestamp: new Date(now - 60_000).toISOString(),
        call_event: {
          kind: "propose",
          from: "bob@example.com/phone",
          sid: "disconnected-call",
          media: { audio: true, video: false },
        },
      }],
      first_id: "mam-propose",
      last_id: "mam-propose",
      is_complete: true,
    };
    let resolveFetch: ((value: typeof page) => void) | null = null;
    const fetchPersonalHistoryPage = mock(() => new Promise<typeof page>((resolve) => {
      resolveFetch = resolve;
    }));
    const client = new BrowserXmppClient(session());
    (client as unknown as { connected: boolean }).connected = true;
    (client as unknown as { xmpp: { fetch_personal_history_page: typeof fetchPersonalHistoryPage } }).xmpp = {
      fetch_personal_history_page: fetchPersonalHistoryPage,
    };

    const hydration = client.hydrateRecentDmCallActivities({ since, maxPages: 1 });
    for (let i = 0; i < 5; i += 1) await Promise.resolve();
    expect(fetchPersonalHistoryPage).toHaveBeenCalledWith(100, { type: "latest", start: since });

    (client as unknown as { connected: boolean }).connected = false;
    resolveFetch?.(page);
    await hydration;

    expect(readDmCallActivity("bob@example.com")).toBeNull();
  });
});
