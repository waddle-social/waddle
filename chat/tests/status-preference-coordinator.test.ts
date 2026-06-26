import { describe, expect, test } from "bun:test";

import { sameMode, StatusPreferenceCoordinator } from "../src/presence/status-preference-coordinator";
import type { PresenceMode } from "../src/presence/effective-show";

const AUTO: PresenceMode = { kind: "automatic" };
const AWAY: PresenceMode = { kind: "manual", status: "away" };
const DND: PresenceMode = { kind: "manual", status: "dnd" };

function harness(initial: PresenceMode = AUTO) {
  let mode = initial;
  let online = true;
  let failNext = false;
  const published: PresenceMode[] = [];
  const coord = new StatusPreferenceCoordinator({
    publish: async (m) => {
      if (!online || failNext) return false;
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
    setFailNext: (f: boolean) => {
      failNext = f;
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
  test("a local pick publishes it", async () => {
    const h = harness();
    h.setMode(AWAY);
    await h.coord.reconcile();
    expect(h.published).toEqual([AWAY]);
  });

  test("does not re-publish an unchanged mode", async () => {
    const h = harness();
    h.setMode(AWAY);
    await h.coord.reconcile();
    await h.coord.reconcile();
    await h.coord.reconcile();
    expect(h.published).toHaveLength(1);
  });

  test("applyRemote adopts the mode without re-publishing (no echo loop)", async () => {
    const h = harness();
    h.coord.applyRemote(DND);
    expect(h.getMode()).toEqual(DND);
    await h.coord.reconcile(); // the store-write would trigger this in the controller
    expect(h.published).toEqual([]);
  });

  test("a publish that fails (offline) does NOT advance the baseline and is retried", async () => {
    const h = harness();
    h.setMode(AWAY);
    h.setOnline(false);
    await h.coord.reconcile(); // attempted, not sent
    expect(h.published).toEqual([]);
    h.setOnline(true);
    await h.coord.reconcile(); // session-ready self-heal retries
    expect(h.published).toEqual([AWAY]);
  });

  test("a publish that REJECTS while online (session not ready) is retried, not recorded as sent", async () => {
    const h = harness();
    h.setMode(AWAY);
    h.setFailNext(true); // online but the publish rejects/returns false
    await h.coord.reconcile();
    expect(h.published).toEqual([]);
    // The baseline was NOT advanced, so the very next reconcile retries even
    // though the mode is unchanged — this is the contract the fix guarantees.
    h.setFailNext(false);
    await h.coord.reconcile();
    expect(h.published).toEqual([AWAY]);
  });

  test("a pick made while a publish was in flight converges on success", async () => {
    // Model an in-flight change: publish AWAY succeeds, but by the time it
    // resolves the mode is DND; the coordinator must publish DND too.
    let resolveFirst: ((v: boolean) => void) | undefined;
    const published: PresenceMode[] = [];
    let mode: PresenceMode = AWAY;
    let first = true;
    const coord = new StatusPreferenceCoordinator({
      publish: (m) => {
        published.push(m);
        if (first) {
          first = false;
          return new Promise<boolean>((res) => {
            resolveFirst = res;
          });
        }
        return Promise.resolve(true);
      },
      mode: () => mode,
      setMode: (m) => {
        mode = m;
      },
    });
    const inFlight = coord.reconcile(); // starts publishing AWAY (pending)
    mode = DND; // pick changes mid-flight
    await coord.reconcile(); // deduped: a publish is in flight, no-op
    expect(published).toEqual([AWAY]);
    resolveFirst?.(true); // AWAY confirmed
    await inFlight; // recurses, publishes DND
    expect(published).toEqual([AWAY, DND]);
  });

  test("suspend stops publishing so a logout reset does not clobber the synced pick", async () => {
    const h = harness();
    h.setMode(AWAY);
    await h.coord.reconcile();
    expect(h.published).toEqual([AWAY]);
    h.coord.suspend();
    h.setMode(AUTO);
    await h.coord.reconcile();
    expect(h.published).toEqual([AWAY]); // automatic was NOT published
  });
});

describe("StatusPreferenceCoordinator.hasPendingLocalPick", () => {
  test("a manual pick is pending even with no baseline yet (offline pick)", () => {
    const h = harness(AWAY);
    expect(h.coord.hasPendingLocalPick()).toBe(true);
  });

  test("the default automatic is NOT pending with no baseline (fresh device adopts remote)", () => {
    const h = harness(AUTO);
    expect(h.coord.hasPendingLocalPick()).toBe(false);
  });

  test("once a baseline exists, only a divergence is pending", async () => {
    const h = harness(AWAY);
    await h.coord.reconcile(); // baseline = AWAY
    expect(h.coord.hasPendingLocalPick()).toBe(false);
    h.setMode(DND);
    expect(h.coord.hasPendingLocalPick()).toBe(true);
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

  test("a manual pick made while disconnected is published, NOT overwritten by the fetched value", async () => {
    // The user picked Away before the socket was ready (no successful publish,
    // so lastPublished is null). On connect the server still holds an older
    // value — but the local pick must win and be published.
    const h = harness(AWAY);
    let fetched = false;
    await h.coord.syncOnConnect(async () => {
      fetched = true;
      return DND; // stale stored value that must NOT clobber the local pick
    });
    expect(fetched).toBe(false); // local pick is pending → fetch is skipped
    expect(h.getMode()).toEqual(AWAY);
    expect(h.published).toEqual([AWAY]);
  });

  test("an unpublished local pick wins over the (stale) server value on reconnect", async () => {
    const h = harness();
    h.setMode(AWAY);
    await h.coord.reconcile(); // baseline = AWAY
    h.setOnline(false);
    h.setMode(DND);
    await h.coord.reconcile(); // offline pick, not sent
    h.setOnline(true);
    let fetched = false;
    await h.coord.syncOnConnect(async () => {
      fetched = true;
      return AWAY;
    });
    expect(fetched).toBe(false);
    expect(h.published).toEqual([AWAY, DND]);
    expect(h.getMode()).toEqual(DND);
  });

  test("a normal reconnect with no local change adopts a peer's update", async () => {
    const h = harness();
    h.setMode(AWAY);
    await h.coord.reconcile(); // baseline = AWAY
    await h.coord.syncOnConnect(async () => DND);
    expect(h.getMode()).toEqual(DND);
    expect(h.published).toEqual([AWAY]); // adopted, not re-published
  });

  test("a local pick made WHILE the fetch is in flight is not clobbered by the stale fetched value", async () => {
    const h = harness(AUTO);
    // The fetch resolves the server's stale value, but the user picks DND
    // before it returns. The pick must win and be published, not be
    // overwritten by the fetched AWAY.
    await h.coord.syncOnConnect(async () => {
      h.setMode(DND); // user picks mid-fetch (the store subscriber would also fire reconcile)
      return AWAY;
    });
    expect(h.getMode()).toEqual(DND);
    expect(h.published).toEqual([DND]);
  });

  test("an inbound update arriving WHILE the fetch is in flight wins over the stale fetched value", async () => {
    const h = harness(AUTO);
    await h.coord.syncOnConnect(async () => {
      h.coord.applyRemote(DND); // a live peer update lands mid-fetch
      return AWAY; // stale snapshot
    });
    expect(h.getMode()).toEqual(DND); // the live update is kept, not overwritten
    expect(h.published).toEqual([]); // applyRemote does not echo
  });

  test("resume re-enables publishing after a suspend/login cycle", async () => {
    const h = harness();
    h.setMode(AWAY);
    await h.coord.reconcile();
    h.coord.suspend();
    h.setMode(AUTO); // logout-time reset
    await h.coord.syncOnConnect(async () => DND);
    expect(h.getMode()).toEqual(DND);
  });
});
