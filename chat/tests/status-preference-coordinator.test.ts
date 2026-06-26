import { describe, expect, test } from "bun:test";

import { sameMode, StatusPreferenceCoordinator } from "../src/presence/status-preference-coordinator";
import type { PresenceMode } from "../src/presence/effective-show";

const AUTO: PresenceMode = { kind: "automatic" };
const AWAY: PresenceMode = { kind: "manual", status: "away" };
const DND: PresenceMode = { kind: "manual", status: "dnd" };

function harness(initial: PresenceMode = AUTO) {
  let mode = initial;
  let online = true;
  const published: PresenceMode[] = [];
  const coord = new StatusPreferenceCoordinator({
    publish: (m) => {
      if (!online) return false;
      published.push(m);
      return true;
    },
    mode: () => mode,
    setMode: (m) => {
      mode = m;
    },
  });
  return {
    coord,
    published,
    getMode: () => mode,
    setMode: (m: PresenceMode) => {
      mode = m;
    },
    setOnline: (o: boolean) => {
      online = o;
    },
  };
}

describe("sameMode", () => {
  test("compares kind and manual status", () => {
    expect(sameMode(AUTO, AUTO)).toBe(true);
    expect(sameMode(AWAY, AWAY)).toBe(true);
    expect(sameMode(AWAY, DND)).toBe(false);
    expect(sameMode(AUTO, AWAY)).toBe(false);
    expect(sameMode(null, null)).toBe(true);
    expect(sameMode(null, AUTO)).toBe(false);
  });
});

describe("StatusPreferenceCoordinator", () => {
  test("a local pick publishes it", () => {
    const h = harness();
    h.setMode(AWAY);
    h.coord.reconcile();
    expect(h.published).toEqual([AWAY]);
  });

  test("does not re-publish an unchanged mode", () => {
    const h = harness();
    h.setMode(AWAY);
    h.coord.reconcile();
    h.coord.reconcile();
    h.coord.reconcile();
    expect(h.published).toHaveLength(1);
  });

  test("applyRemote adopts the mode without re-publishing (no echo loop)", () => {
    const h = harness();
    h.coord.applyRemote(DND);
    expect(h.getMode()).toEqual(DND);
    h.coord.reconcile(); // the store-write would trigger this in the controller
    expect(h.published).toEqual([]);
  });

  test("a pick made while offline is retried and published on the next reconcile", () => {
    const h = harness();
    // Establish a baseline first (so this isn't the fresh-login path).
    h.setMode(AWAY);
    h.coord.reconcile();
    expect(h.published).toEqual([AWAY]);
    // Now go offline and pick DND.
    h.setOnline(false);
    h.setMode(DND);
    h.coord.reconcile(); // attempted, not sent
    expect(h.published).toEqual([AWAY]);
    // Reconnect: session-ready reconcile flushes it.
    h.setOnline(true);
    h.coord.reconcile();
    expect(h.published).toEqual([AWAY, DND]);
  });

  test("suspend stops publishing so a logout reset does not clobber the synced pick", () => {
    const h = harness();
    h.setMode(AWAY);
    h.coord.reconcile();
    expect(h.published).toEqual([AWAY]);
    // Logout: suspend, then the local reset-to-automatic fires reconcile.
    h.coord.suspend();
    h.setMode(AUTO);
    h.coord.reconcile();
    expect(h.published).toEqual([AWAY]); // automatic was NOT published
  });
});

describe("StatusPreferenceCoordinator.syncOnConnect", () => {
  test("fresh login adopts the server's stored pick", async () => {
    const h = harness(AUTO);
    await h.coord.syncOnConnect(async () => AWAY);
    expect(h.getMode()).toEqual(AWAY);
    expect(h.published).toEqual([]); // adopting must not echo
  });

  test("fresh login with nothing stored keeps the default and publishes nothing", async () => {
    const h = harness(AUTO);
    await h.coord.syncOnConnect(async () => null);
    expect(h.getMode()).toEqual(AUTO);
    expect(h.published).toEqual([]);
  });

  test("a local manual pick before the first connect is published when nothing is stored", async () => {
    const h = harness(AWAY); // user picked Away before the socket was ready
    await h.coord.syncOnConnect(async () => null);
    expect(h.published).toEqual([AWAY]);
  });

  test("an unpublished local pick wins over the (stale) server value on reconnect", async () => {
    const h = harness();
    // Live: publish AWAY (baseline = AWAY).
    h.setMode(AWAY);
    h.coord.reconcile();
    // Offline pick DND (not sent).
    h.setOnline(false);
    h.setMode(DND);
    h.coord.reconcile();
    h.setOnline(true);
    // Reconnect: server still holds the stale AWAY, but our pending DND wins.
    let fetched = false;
    await h.coord.syncOnConnect(async () => {
      fetched = true;
      return AWAY;
    });
    expect(fetched).toBe(false); // skipped the fetch; pushed local intent
    expect(h.published).toEqual([AWAY, DND]);
    expect(h.getMode()).toEqual(DND);
  });

  test("a normal reconnect with no local change adopts a peer's update", async () => {
    const h = harness();
    h.setMode(AWAY);
    h.coord.reconcile(); // baseline = AWAY
    // Another device switched to DND while we were briefly gone.
    await h.coord.syncOnConnect(async () => DND);
    expect(h.getMode()).toEqual(DND);
    expect(h.published).toEqual([AWAY]); // adopted, not re-published
  });

  test("resume re-enables publishing after a suspend/login cycle", async () => {
    const h = harness();
    h.setMode(AWAY);
    h.coord.reconcile();
    h.coord.suspend();
    h.setMode(AUTO); // logout-time reset
    // New login on the same tab; server has DND from another device.
    await h.coord.syncOnConnect(async () => DND);
    expect(h.getMode()).toEqual(DND);
  });
});
