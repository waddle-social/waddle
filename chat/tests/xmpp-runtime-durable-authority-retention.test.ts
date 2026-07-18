import { describe, expect, test } from "bun:test";
import {
  OUTBOUND_CLAIM_LEASE_MS,
  SM_SNAPSHOT_RETENTION_MS,
  committedOrThrow,
  createOutboundClaim,
  type DurableOutboundStore,
  type OutboundOwnerActivation,
  type OutboundOwnerHint,
} from "../src/lib/xmpp-runtime/durable-contract";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime/memory-durable-store";
import type { PersistedQueuedDmMessage } from "../src/lib/outbound-queue-store";
import type { PersistedSmResumeState } from "../src/lib/xmpp/sm-resume-types";
import { recordingDurableStore } from "./durable-account-repository-test-support";

const ACCOUNT = "alice@example.com";

function directMessage(
  id: string,
  body = "hello",
  createdAt = "2026-07-17T00:00:00.000Z",
): PersistedQueuedDmMessage {
  return {
    kind: "dm",
    id,
    createdAt,
    peerJid: "bob@example.com",
    body,
  };
}

async function activate(
  store: DurableOutboundStore,
  hint: OutboundOwnerHint = {
    ownerId: crypto.randomUUID(),
    ownerInstanceId: crypto.randomUUID(),
  },
): Promise<OutboundOwnerActivation> {
  return committedOrThrow("activate", await store.claimOwner(ACCOUNT, hint));
}

function smState(previd: string): PersistedSmResumeState {
  return {
    previd,
    inboundH: 4,
    outboundH: 7,
    maxResumeSeconds: 300,
    unhandledOutboundEntries: [],
  };
}

function expectCounterExhaustion(outcome: {
  kind: string;
  reason?: string;
  cause?: unknown;
}): void {
  expect(outcome.kind).toBe("failed");
  expect(outcome.reason).toBe("aborted");
  expect(outcome.cause).toBeInstanceOf(DOMException);
  expect((outcome.cause as DOMException).name).toBe("AbortError");
}

describe("unified XMPP runtime authority", () => {
  test("queued authority mutation samples time after waiting and rejects an expired owner", async () => {
    let now = 0;
    let blockNext = false;
    let releaseBlocked: () => void = () => {
      throw new Error("queued authority mutation did not reach its hook");
    };
    const store = new MemoryDurableOutboundStore(
      { now: () => now },
      async () => {
        if (!blockNext) return;
        blockNext = false;
        await new Promise<void>((resolve) => {
          releaseBlocked = resolve;
        });
      },
    );
    const owner = (await activate(store, {
      ownerId: "queued-owner",
      ownerInstanceId: "queued-instance",
    })).fence;
    await store.persistReady(ACCOUNT, directMessage("queued-expiry"));

    blockNext = true;
    const pendingClaim = store.claimHead(
      ACCOUNT,
      { kind: "direct" },
      createOutboundClaim(owner, 1, "sending"),
    );
    await Promise.resolve();
    now = 45_001;
    releaseBlocked();

    expect(committedOrThrow(
      "claim-after-queued-expiry",
      await pendingClaim,
    ).kind).toBe("fenced");
    const scan = committedOrThrow(
      "scan-after-expiry",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    expect(scan.entries[0]?.state.kind).toBe("ready");
  });

  test("invalid negative authority samples fail without committing metadata", async () => {
    let now = 10_000;
    const { store, repository } = recordingDurableStore({ now: () => now });
    await activate(store, {
      ownerId: "invalid-clock-owner",
      ownerInstanceId: "invalid-clock-instance",
    });
    const before = repository.inspect(ACCOUNT);

    now = -1;
    expectCounterExhaustion(await store.revision(ACCOUNT));

    expect(repository.inspect(ACCOUNT)).toEqual(before);
  });

  test("near-limit authority time rejects lease overflow without creating an owner", async () => {
    const now = Number.MAX_SAFE_INTEGER - OUTBOUND_CLAIM_LEASE_MS + 1;
    const { store, repository } = recordingDurableStore({ now: () => now });

    expectCounterExhaustion(await store.claimOwner(ACCOUNT, {
      ownerId: "deadline-overflow-owner",
      ownerInstanceId: "deadline-overflow-instance",
    }));

    expect(repository.has(ACCOUNT)).toBe(false);
  });

  test("backward wall-clock movement cannot reopen expired owner authority", async () => {
    let now = 1_000;
    const store = new MemoryDurableOutboundStore({ now: () => now });
    const hint = {
      ownerId: "clock-owner",
      ownerInstanceId: "clock-instance",
    };
    const original = (await activate(store, hint)).fence;

    now = 50_000;
    await store.revision(ACCOUNT);
    now = 500;
    const replacement = await activate(store, hint);

    expect(replacement.fence.ownerId).not.toBe(original.ownerId);
    expect(committedOrThrow(
      "expired-renewal",
      await store.renewOwner(original),
    )).toBe(false);
  });

  test("expired same-instance reclaim allocates a new generation and permanently fences the old one", async () => {
    let now = 0;
    const store = new MemoryDurableOutboundStore({ now: () => now });
    const hint = {
      ownerId: "same-instance-owner",
      ownerInstanceId: "same-instance",
    };
    const predecessor = (await activate(store, hint)).fence;
    await store.persistReady(ACCOUNT, directMessage("same-instance-row"));

    now = 45_001;
    const successor = (await activate(store, hint)).fence;

    expect(successor).toMatchObject({
      ownerInstanceId: predecessor.ownerInstanceId,
      ownerGeneration: predecessor.ownerGeneration + 1,
      authorityEpoch: predecessor.authorityEpoch,
    });
    expect(successor.ownerId).not.toBe(predecessor.ownerId);
    expect(committedOrThrow(
      "renew-expired-predecessor",
      await store.renewOwner(predecessor),
    )).toBe(false);
    expect(committedOrThrow(
      "claim-with-expired-predecessor",
      await store.claimHead(
        ACCOUNT,
        { kind: "direct" },
        createOutboundClaim(predecessor, 1, "sending"),
      ),
    ).kind).toBe("fenced");
  });

  test("clock rollback commits one fencing epoch before the stale mutation rebounds", async () => {
    let now = 10_000;
    const store = new MemoryDurableOutboundStore({ now: () => now });
    const hint = {
      ownerId: "rollback-owner",
      ownerInstanceId: "rollback-instance",
    };
    const original = (await activate(store, hint)).fence;
    await store.persistReady(ACCOUNT, directMessage("rollback-row"));
    const revisionBeforeRollback = committedOrThrow(
      "revision-before-rollback",
      await store.revision(ACCOUNT),
    );

    now = 1_000;
    const fenced = committedOrThrow(
      "first-stale-mutation",
      await store.claimHead(
        ACCOUNT,
        { kind: "direct" },
        createOutboundClaim(original, 1, "sending"),
      ),
    );
    expect(fenced.kind).toBe("fenced");
    expect(committedOrThrow(
      "revision-after-rollback",
      await store.revision(ACCOUNT),
    )).toBe(revisionBeforeRollback + 1);

    const rebound = await activate(store, hint);
    expect(rebound.fence).toMatchObject({
      ownerInstanceId: original.ownerInstanceId,
      ownerGeneration: original.ownerGeneration + 1,
      authorityEpoch: original.authorityEpoch + 1,
    });
    expect(rebound.fence.ownerId).not.toBe(original.ownerId);
    const claimed = committedOrThrow(
      "claim-after-rebound",
      await store.claimHead(
        ACCOUNT,
        { kind: "direct" },
        createOutboundClaim(rebound.fence, 2, "sending"),
      ),
    );
    expect(claimed.kind).toBe("claimed");
  });

  test("ordinary authority clock sampling does not advance the domain revision", async () => {
    let now = 20_000;
    const store = new MemoryDurableOutboundStore({ now: () => now });
    await activate(store, {
      ownerId: "revision-owner",
      ownerInstanceId: "revision-instance",
    });
    const revision = committedOrThrow(
      "revision-before-sample",
      await store.revision(ACCOUNT),
    );
    now += 1;
    expect(committedOrThrow(
      "revision-during-sample",
      await store.revision(ACCOUNT),
    )).toBe(revision);
    expect(committedOrThrow(
      "revision-after-sample",
      await store.revision(ACCOUNT),
    )).toBe(revision);
  });

  test("revision and empty scan return the revision committed by first rollback fencing", async () => {
    let now = 20_000;
    const store = new MemoryDurableOutboundStore({ now: () => now });
    const initialRevision = committedOrThrow(
      "initial-revision",
      await store.revision(ACCOUNT),
    );

    now = 1_000;
    const rollbackRevision = committedOrThrow(
      "rollback-revision",
      await store.revision(ACCOUNT),
    );
    expect(rollbackRevision).toBe(initialRevision + 1);

    now = 30_000;
    const settledRevision = committedOrThrow(
      "settled-revision",
      await store.revision(ACCOUNT),
    );
    now = 2_000;
    const scan = committedOrThrow(
      "empty-scan-rollback",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    expect(scan.entries).toEqual([]);
    expect(scan.revision).toBe(settledRevision + 1);
    expect(committedOrThrow(
      "revision-after-empty-scan",
      await store.revision(ACCOUNT),
    )).toBe(scan.revision);
  });

  test("rollback plus a domain mutation advances revision exactly once", async () => {
    let now = 30_000;
    const store = new MemoryDurableOutboundStore({ now: () => now });
    await store.persistReady(ACCOUNT, directMessage("before-combined-rollback"));
    const revision = committedOrThrow(
      "revision-before-combined-rollback",
      await store.revision(ACCOUNT),
    );

    now = 1_000;
    const persisted = committedOrThrow(
      "persist-during-rollback",
      await store.persistReady(ACCOUNT, directMessage("during-combined-rollback")),
    );
    expect(persisted.kind).toBe("inserted");
    expect(committedOrThrow(
      "revision-after-combined-rollback",
      await store.revision(ACCOUNT),
    )).toBe(revision + 1);
  });

  test("resume-expired SM state retains its owner until snapshot retention expires", async () => {
    const startedAt = 1_000;
    let now = startedAt;
    const { store, repository } = recordingDurableStore({ now: () => now });
    const owner = (await activate(store, {
      ownerId: "retention-live-owner",
      ownerInstanceId: "retention-live-instance",
    })).fence;
    const saved = committedOrThrow(
      "retention-live-save",
      await store.saveSm(owner, null, smState("retention-live"), now),
    );
    if (saved.kind !== "applied") throw new Error("expected retained SM state");

    now = startedAt + SM_SNAPSHOT_RETENTION_MS + 1;
    committedOrThrow(
      "retention-live-scan",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    const retentionLive = repository.inspect(ACCOUNT);
    expect(retentionLive.smSnapshots[owner.ownerId]).toBeDefined();
    expect(retentionLive.owners[owner.ownerId]).toBeDefined();

    now = startedAt + SM_SNAPSHOT_RETENTION_MS + 300_000 + 1;
    committedOrThrow(
      "retention-expired-scan",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    const retentionExpired = repository.inspect(ACCOUNT);
    expect(retentionExpired.smSnapshots[owner.ownerId]).toBeUndefined();
    expect(retentionExpired.owners[owner.ownerId]).toBeUndefined();
  });

  test("SM tombstones retain their exact owner until the tombstone TTL expires", async () => {
    const startedAt = 2_000;
    let now = startedAt;
    const { store, repository } = recordingDurableStore({ now: () => now });
    const owner = (await activate(store, {
      ownerId: "tombstone-retention-owner",
      ownerInstanceId: "tombstone-retention-instance",
    })).fence;
    const cleared = committedOrThrow(
      "tombstone-retention-clear",
      await store.clearSm(owner, null),
    );
    if (cleared.kind !== "applied") throw new Error("expected SM tombstone");

    now = startedAt + SM_SNAPSHOT_RETENTION_MS - 1;
    committedOrThrow(
      "tombstone-retained-scan",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    const retained = repository.inspect(ACCOUNT);
    expect(retained.smSnapshots[owner.ownerId]?.state).toBeNull();
    expect(retained.owners[owner.ownerId]).toBeDefined();

    now = startedAt + SM_SNAPSHOT_RETENTION_MS + 1;
    committedOrThrow(
      "tombstone-expired-scan",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    const expired = repository.inspect(ACCOUNT);
    expect(expired.smSnapshots[owner.ownerId]).toBeUndefined();
    expect(expired.owners[owner.ownerId]).toBeUndefined();
  });

  test("renewed exact owners protect retention-expired SM state and tombstones only until lease expiry", async () => {
    // A resumable snapshot is retained for its advertised 300-second resume
    // window plus the eight-day terminal-reference horizon. Start beyond that
    // combined deadline so only the exact renewed owner can protect either
    // record in the first scan.
    let now = SM_SNAPSHOT_RETENTION_MS + 300_000 + 10_000;
    const { store, repository } = recordingDurableStore({ now: () => now });
    const snapshotOwner = (await activate(store, {
      ownerId: "renewed-old-snapshot-owner",
      ownerInstanceId: "renewed-old-snapshot-instance",
    })).fence;
    const tombstoneOwner = (await activate(store, {
      ownerId: "renewed-old-tombstone-owner",
      ownerInstanceId: "renewed-old-tombstone-instance",
    })).fence;
    const saved = committedOrThrow(
      "renewed-old-snapshot-save",
      await store.saveSm(snapshotOwner, null, smState("renewed-old"), now),
    );
    if (saved.kind !== "applied") throw new Error("expected old SM snapshot");
    const cleared = committedOrThrow(
      "renewed-old-tombstone-clear",
      await store.clearSm(tombstoneOwner, null),
    );
    if (cleared.kind !== "applied") throw new Error("expected old SM tombstone");

    repository.mutate(ACCOUNT, (seeded) => {
      seeded.smSnapshots[snapshotOwner.ownerId]!.savedAt = 0;
      seeded.smSnapshots[tombstoneOwner.ownerId]!.savedAt = 0;
    });
    expect(committedOrThrow(
      "renew-old-snapshot-owner",
      await store.renewOwner(snapshotOwner),
    )).toBe(true);
    expect(committedOrThrow(
      "renew-old-tombstone-owner",
      await store.renewOwner(tombstoneOwner),
    )).toBe(true);

    committedOrThrow(
      "scan-renewed-old-sm",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    const protectedRecords = repository.inspect(ACCOUNT).smSnapshots;
    expect(protectedRecords[snapshotOwner.ownerId]?.state).toBeDefined();
    expect(protectedRecords[tombstoneOwner.ownerId]?.state).toBeNull();

    now += OUTBOUND_CLAIM_LEASE_MS + 1;
    committedOrThrow(
      "scan-expired-old-sm",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    const expiredRecords = repository.inspect(ACCOUNT).smSnapshots;
    expect(expiredRecords[snapshotOwner.ownerId]).toBeUndefined();
    expect(expiredRecords[tombstoneOwner.ownerId]).toBeUndefined();
  });

});
