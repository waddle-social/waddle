import { describe, expect, test } from "bun:test";
import { getConnectionNoticeCopy } from "../src/lib/connection-notice";

describe("connection notice copy", () => {
  test("surfaces reconnecting guidance for queued messages", () => {
    const notice = getConnectionNoticeCopy({
      status: { state: "reconnecting", detail: "Connection lost, reconnecting..." },
      queuedMessageCount: 2,
      showReconnected: false,
    });

    expect(notice).toEqual({
      tone: "reconnecting",
      shortLabel: "reconnecting",
      title: "Reconnecting…",
      body: "Trying again now. 2 queued messages will send once you're connected again.",
    });
  });

  test("keeps ordinary offline guidance unchanged", () => {
    const notice = getConnectionNoticeCopy({
      status: { state: "offline", detail: "" },
      queuedMessageCount: 2,
      showReconnected: false,
    });

    expect(notice).toEqual({
      tone: "offline",
      shortLabel: "offline",
      title: "Disconnected",
      body: "You're offline. 2 queued messages will send once you're connected again.",
    });
  });

  test("surfaces a reconnect affordance for superseded sessions", () => {
    const notice = getConnectionNoticeCopy({
      status: { state: "offline", detail: "This session was resumed in another tab.", kind: "superseded" },
      queuedMessageCount: 2,
      showReconnected: false,
    });

    expect(notice).toEqual({
      tone: "offline",
      shortLabel: "reconnect",
      title: "Session resumed in another tab",
      body: "This session was resumed in another tab. Reconnect to continue from this tab.",
      actionLabel: "Reconnect",
      actionKind: "recover-superseded",
    });
  });

  test("communicates a calm recovery once the connection returns", () => {
    const notice = getConnectionNoticeCopy({
      status: { state: "online", detail: "Connection resumed" },
      queuedMessageCount: 1,
      showReconnected: true,
    });

    expect(notice).toEqual({
      tone: "reconnected",
      shortLabel: "back online",
      title: "Back online",
      body: "1 queued message is sending now.",
    });
  });

  test("keeps queued message expectations explicit for fatal errors", () => {
    const notice = getConnectionNoticeCopy({
      status: { state: "error", detail: "Session expired. Please log in again." },
      queuedMessageCount: 0,
      showReconnected: false,
    });

    expect(notice).toEqual({
      tone: "error",
      shortLabel: "needs attention",
      title: "Connection needs attention",
      body: "Your session needs attention. Sign in again to restore live messaging. Any queued messages will stay here and send once you're connected again.",
    });
  });

  test("stays hidden during a steady online session", () => {
    expect(getConnectionNoticeCopy({
      status: { state: "online", detail: "Connection ready" },
      queuedMessageCount: 0,
      showReconnected: false,
    })).toBeNull();
  });
});
