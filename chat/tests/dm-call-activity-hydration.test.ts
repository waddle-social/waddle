import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import {
  clearDmCallActivities,
  readDmCallActivity,
} from "../src/lib/calls/dm-call-activity";
import type { WaddleSession } from "../src/lib/server-auth";
import type { ResumePersistence } from "../src/lib/xmpp/resume-persistence";

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

  test("reuses and consumes the persisted SM resource so same-resource joins remain valid after reload", () => {
    let sm: ReturnType<ResumePersistence["loadSm"]> = {
      previd: "prev",
      inboundH: 1,
      outboundH: 2,
      resource: "web-existing-resource",
    };
    const consumedStates: NonNullable<typeof sm>[] = [];
    const persistence: ResumePersistence = {
      loadCatchup: () => null,
      saveCatchup: () => undefined,
      clearCatchup: () => undefined,
      loadSm: () => sm,
      consumeSm: () => {
        const current = sm;
        sm = null;
        if (current) consumedStates.push(current);
        return current;
      },
      saveSm: () => undefined,
      clearSm: () => {
        sm = null;
      },
      preparePagehideHandoff: () => undefined,
      loadJoinedRooms: () => [],
      saveJoinedRooms: () => undefined,
      clearJoinedRooms: () => undefined,
    };

    const client = new BrowserXmppClient(session(), persistence);
    const secondClient = new BrowserXmppClient(session(), persistence);

    expect(client.fullJid).toBe("alice@example.com/web-existing-resource");
    expect(consumedStates).toHaveLength(1);
    expect(secondClient.fullJid).not.toBe("alice@example.com/web-existing-resource");
  });

  test("does not publish transient reconnect resources into shared SM persistence", () => {
    let sm: ReturnType<ResumePersistence["loadSm"]> = null;
    const savedStates: NonNullable<typeof sm>[] = [];
    const persistence: ResumePersistence = {
      loadCatchup: () => null,
      saveCatchup: () => undefined,
      clearCatchup: () => undefined,
      loadSm: () => sm,
      consumeSm: () => {
        const current = sm;
        sm = null;
        return current;
      },
      saveSm: (state) => {
        savedStates.push(state);
        sm = state;
      },
      clearSm: () => {
        sm = null;
      },
      preparePagehideHandoff: () => undefined,
      loadJoinedRooms: () => [],
      saveJoinedRooms: () => undefined,
      clearJoinedRooms: () => undefined,
    };
    const client = new BrowserXmppClient(session(), persistence);
    const resource = client.fullJid.split("/")[1];
    const xmpp = {
      get_resume_state_handle: () => ({ free: () => undefined }),
      get_resume_state: () => ({ previd: "prev", inboundH: 1, outboundH: 2 }),
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).connected = true;

    (client as unknown as { handleDisconnected: (xmpp: typeof xmpp) => void }).handleDisconnected(xmpp);
    (client as unknown as { clearReconnectTimer: () => void }).clearReconnectTimer();

    const secondClient = new BrowserXmppClient(session(), persistence);

    expect(savedStates).toEqual([]);
    expect(secondClient.fullJid).not.toBe(`alice@example.com/${resource}`);
  });

  test("pagehide persists real SM state with resource, including reconnect fallback state", () => {
    let sm: ReturnType<ResumePersistence["loadSm"]> = null;
    const savedStates: NonNullable<typeof sm>[] = [];
    const pagehideOrder: string[] = [];
    const persistence: ResumePersistence = {
      loadCatchup: () => null,
      saveCatchup: () => undefined,
      clearCatchup: () => undefined,
      loadSm: () => sm,
      consumeSm: () => {
        const current = sm;
        sm = null;
        return current;
      },
      saveSm: (state) => {
        pagehideOrder.push("save-sm");
        savedStates.push(state);
        sm = state;
      },
      clearSm: () => {
        sm = null;
      },
      preparePagehideHandoff: () => {
        pagehideOrder.push("prepare-handoff");
      },
      loadJoinedRooms: () => [],
      saveJoinedRooms: () => undefined,
      clearJoinedRooms: () => undefined,
    };
    const client = new BrowserXmppClient(session(), persistence);
    const resource = client.fullJid.split("/")[1];
    const requestStreamManagementAck = mock(async () => {
      pagehideOrder.push("request-ack");
    });
    const xmpp = {
      get_resume_state_handle: () => ({ free: () => undefined }),
      get_resume_state: () => {
        pagehideOrder.push("snapshot-sm");
        return { previd: "prev-live", inboundH: 7, outboundH: 9 };
      },
      request_stream_management_ack: requestStreamManagementAck,
    };
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).xmpp = xmpp;
    (client as unknown as { xmpp: typeof xmpp; connected: boolean }).connected = true;

    client.prepareForPageHide();
    expect(pagehideOrder).toEqual([
      "request-ack",
      "snapshot-sm",
      "prepare-handoff",
      "save-sm",
    ]);
    (client as unknown as { handleDisconnected: (xmpp: typeof xmpp) => void }).handleDisconnected(xmpp);
    (client as unknown as { clearReconnectTimer: () => void }).clearReconnectTimer();
    client.persistResumeStateForPageHide();

    expect(savedStates).toEqual([
      { previd: "prev-live", inboundH: 7, outboundH: 9, resource },
      { previd: "prev-live", inboundH: 7, outboundH: 9, resource },
    ]);
    expect(requestStreamManagementAck).toHaveBeenCalledTimes(1);
  });

  test("hydrates active call state from personal MAM pages without loading the DM timeline", async () => {
    const now = Date.now();
    const timestamp = (offsetMs: number) => new Date(now + offsetMs).toISOString();
    const latestPage = {
      messages: [
        {
          mam_id: "mam-proceed",
          from: "alice@example.com",
          to: "bob@example.com",
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
          from: "bob@example.com",
          to: "alice@example.com",
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

  test("hydrates a newer terminal marker as silent stale-call cleanup", async () => {
    const acceptedAt = "2026-05-26T12:00:00.000Z";
    const finishedAt = "2026-05-26T12:03:00.000Z";
    const latestPage = {
      messages: [
        {
          mam_id: "mam-finish",
          from: "bob@example.com",
          to: "alice@example.com",
          timestamp: finishedAt,
          call_event: {
            kind: "finish",
            from: "bob@example.com/phone",
            sid: "ended-while-away",
          },
        },
      ],
      first_id: "mam-finish",
      last_id: "mam-finish",
      is_complete: false,
    };
    const olderPage = {
      messages: [
        {
          mam_id: "mam-accepted",
          from: "bob@example.com",
          to: "alice@example.com",
          timestamp: acceptedAt,
          call_event: {
            kind: "session-accept",
            from: "bob@example.com/phone",
            to: "alice@example.com/web",
            sid: "ended-while-away",
            media: { audio: true, video: true },
            join: {
              url: "wss://livekit.example.test",
              room: "dm-ended-while-away",
              identity: "alice@example.com/web",
              token: "opaque-token",
            },
          },
        },
      ],
      first_id: "mam-accepted",
      last_id: "mam-accepted",
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

    await client.hydrateRecentDmCallActivities({
      since: "2026-05-26T11:00:00.000Z",
      maxPages: 2,
    });

    expect(fetchPersonalHistoryPage).toHaveBeenNthCalledWith(1, 100, {
      type: "latest",
      start: "2026-05-26T11:00:00.000Z",
    });
    expect(fetchPersonalHistoryPage).toHaveBeenNthCalledWith(2, 100, {
      type: "before",
      before: "mam-finish",
      start: "2026-05-26T11:00:00.000Z",
    });
    expect(readDmCallActivity("bob@example.com")).toBeNull();
  });

  test("does not apply hydrated pages after the connected XMPP session changes", async () => {
    const now = Date.now();
    const timestamp = new Date(now - 60_000).toISOString();
    const page = {
      messages: [{
        mam_id: "mam-propose",
        from: "bob@example.com",
        to: "alice@example.com",
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
        from: "bob@example.com",
        to: "alice@example.com",
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
