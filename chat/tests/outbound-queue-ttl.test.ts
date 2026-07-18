/**
 * Durable outbound queue retention.
 *
 * IndexedDB is the canonical queue and the synchronous localStorage view is
 * only a paint projection. Retention therefore runs transactionally through
 * DurableOutboundStore: projection reads must never independently delete work
 * that may still be claimed by a live sender or awaiting terminal handling.
 */
import { describe, expect, test } from "bun:test";
import type { PersistedQueuedDmMessage } from "../src/lib/outbound-queue-store";
import { QUEUE_TTL_MS } from "../src/lib/outbound-queue-store";
import {
  committedOrThrow,
  createOutboundClaim,
} from "../src/lib/xmpp-runtime/durable-contract";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime-durable-store";

const ACCOUNT = "alice@example.com";
const NOW = Date.parse("2026-07-17T12:00:00.000Z");
const DAY_MS = 24 * 60 * 60 * 1000;
const CUTOFF = NOW - QUEUE_TTL_MS;

function directMessage(
  id: string,
  createdAt: string,
): PersistedQueuedDmMessage {
  return {
    kind: "dm",
    id,
    createdAt,
    peerJid: "bob@example.com",
    body: id,
  };
}

async function activate(store: MemoryDurableOutboundStore) {
  return committedOrThrow(
    "activate-test-owner",
    await store.claimOwner(ACCOUNT, {
      ownerId: "ttl-owner",
      ownerInstanceId: crypto.randomUUID(),
    }),
  ).fence;
}

describe("durable outbound queue TTL", () => {
  test("ready entries within the 7-day window are preserved", async () => {
    const store = new MemoryDurableOutboundStore({ now: () => NOW });
    await store.persistReady(
      ACCOUNT,
      directMessage("dm-fresh", new Date(NOW - 3 * DAY_MS).toISOString()),
    );

    const scan = committedOrThrow(
      "scan-fresh",
      await store.scanAndPrune(ACCOUNT, CUTOFF),
    );
    expect(scan.pruned).toEqual([]);
    expect(scan.entries.map((entry) => entry.identity.messageId)).toEqual(["dm-fresh"]);
  });

  test("stale ready entries are deleted from canonical storage", async () => {
    const store = new MemoryDurableOutboundStore({ now: () => NOW });
    await store.persistReady(
      ACCOUNT,
      directMessage("dm-stale", new Date(NOW - 30 * DAY_MS).toISOString()),
    );

    const scan = committedOrThrow(
      "scan-stale",
      await store.scanAndPrune(ACCOUNT, CUTOFF),
    );
    expect(scan.pruned.map((identity) => identity.messageId)).toEqual(["dm-stale"]);
    expect(scan.entries).toEqual([]);
    expect(committedOrThrow("list-after-prune", await store.list(ACCOUNT))).toEqual([]);
  });

  test("a mixed scan prunes only stale canonical rows", async () => {
    const store = new MemoryDurableOutboundStore({ now: () => NOW });
    await store.persistReady(
      ACCOUNT,
      directMessage("dm-stale", new Date(NOW - 30 * DAY_MS).toISOString()),
    );
    await store.persistReady(
      ACCOUNT,
      directMessage("dm-fresh", new Date(NOW - DAY_MS).toISOString()),
    );

    const scan = committedOrThrow(
      "scan-mixed",
      await store.scanAndPrune(ACCOUNT, CUTOFF),
    );
    expect(scan.pruned.map((identity) => identity.messageId)).toEqual(["dm-stale"]);
    expect(scan.entries.map((entry) => entry.identity.messageId)).toEqual(["dm-fresh"]);
  });

  test("a stale row with a live claim is immune to pruning", async () => {
    const store = new MemoryDurableOutboundStore({ now: () => NOW });
    const owner = await activate(store);
    const persisted = committedOrThrow(
      "persist-live-claim",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("dm-live", new Date(NOW - 30 * DAY_MS).toISOString()),
        createOutboundClaim(owner, 1, "sending"),
      ),
    );
    expect(persisted.kind).toBe("claimed");

    const scan = committedOrThrow(
      "scan-live-claim",
      await store.scanAndPrune(ACCOUNT, CUTOFF),
    );
    expect(scan.pruned).toEqual([]);
    expect(scan.entries[0]?.state.kind).toBe("claimed");
  });

  test("a stale row is pruned after its claim expires", async () => {
    let now = NOW;
    const store = new MemoryDurableOutboundStore({ now: () => now });
    const owner = await activate(store);
    const persisted = committedOrThrow(
      "persist-expiring-claim",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("dm-expired", new Date(NOW - 30 * DAY_MS).toISOString()),
        createOutboundClaim(owner, 1, "sending"),
      ),
    );
    if (persisted.kind !== "claimed") throw new Error("expected claimed row");
    now = persisted.claim.leaseUntil + 1;

    const scan = committedOrThrow(
      "scan-expired-claim",
      await store.scanAndPrune(ACCOUNT, CUTOFF),
    );
    expect(scan.pruned.map((identity) => identity.messageId)).toEqual(["dm-expired"]);
    expect(scan.entries).toEqual([]);
  });

  test("terminal rows are immune until their terminal intent is applied", async () => {
    const store = new MemoryDurableOutboundStore({ now: () => NOW });
    const owner = await activate(store);
    const persisted = committedOrThrow(
      "persist-terminal",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("dm-terminal", new Date(NOW - 30 * DAY_MS).toISOString()),
        createOutboundClaim(owner, 1, "sending"),
      ),
    );
    if (persisted.kind !== "claimed") throw new Error("expected claimed row");
    const terminal = committedOrThrow(
      "record-terminal",
      await store.recordTerminal(
        persisted.entry.identity,
        "ack",
        persisted.claim,
      ),
    );
    expect(terminal.kind).toBe("recorded");

    const scan = committedOrThrow(
      "scan-terminal",
      await store.scanAndPrune(ACCOUNT, CUTOFF),
    );
    expect(scan.pruned).toEqual([]);
    expect(scan.entries[0]?.state.kind).toBe("terminal");
  });

  test("unparseable createdAt fails closed and remains durable", async () => {
    const store = new MemoryDurableOutboundStore({ now: () => NOW });
    await store.persistReady(
      ACCOUNT,
      directMessage("dm-mystery", "not-a-date"),
    );

    const scan = committedOrThrow(
      "scan-invalid-created-at",
      await store.scanAndPrune(ACCOUNT, CUTOFF),
    );
    expect(scan.pruned).toEqual([]);
    expect(scan.entries.map((entry) => entry.identity.messageId)).toEqual(["dm-mystery"]);
  });
});
