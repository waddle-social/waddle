import { describe, expect, test } from "bun:test";
import {
  DurablePredecessorCapacityError,
  OUTBOUND_CLAIM_LEASE_MS,
  RETAINED_PREDECESSOR_LIMIT,
  SM_SNAPSHOT_RETENTION_MS,
  committedOrThrow,
  createOutboundClaim,
  type OutboundOwnerActivation,
  type OutboundOwnerHint,
} from "../src/lib/xmpp-runtime/durable-contract";
import { validatePersistedRuntimeAccount } from "../src/lib/xmpp-runtime/durable-codec";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime-durable-store";
import type { PersistedQueuedDmMessage } from "../src/lib/outbound-queue-store";
import type { PersistedSmResumeState } from "../src/lib/xmpp/sm-resume-types";

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
  store: MemoryDurableOutboundStore,
  hint: OutboundOwnerHint = {
    ownerId: crypto.randomUUID(),
    ownerInstanceId: crypto.randomUUID(),
  },
): Promise<OutboundOwnerActivation> {
  return committedOrThrow("activate", await store.claimOwner(ACCOUNT, hint));
}

async function handoffTo(
  store: MemoryDurableOutboundStore,
  predecessor: OutboundOwnerActivation["fence"],
  successorInstanceId: string,
): Promise<OutboundOwnerActivation> {
  const loaded = committedOrThrow(
    "load-before-handoff",
    await store.loadSm(predecessor),
  );
  if (loaded.kind !== "loaded") throw new Error("expected live predecessor");
  const prepared = committedOrThrow(
    "prepare-handoff",
    await store.preparePagehideHandoff(
      predecessor,
      loaded.version,
      crypto.randomUUID(),
      null,
    ),
  );
  if (prepared.kind !== "applied") throw new Error("expected handoff preparation");
  return activate(store, {
    ownerId: predecessor.ownerId,
    ownerInstanceId: successorInstanceId,
    handoffToken: prepared.value.handoff.token,
  });
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

function expectCorruptRuntimeAccount(value: unknown): void {
  let failure: unknown;
  try {
    validatePersistedRuntimeAccount(value, ACCOUNT);
  } catch (error) {
    failure = error;
  }
  expect(failure).toBeInstanceOf(DOMException);
  expect((failure as DOMException).name).toBe("DataError");
}

type MutableRuntimeAccountForTest = {
  revision: number;
  authorityEpoch: number;
  lastWallClockSampleMs: number;
  nextOwnerGeneration: number;
  owners: Record<string, {
    ownerGeneration: number;
    authorityEpoch: number;
    leaseUntil: number;
  }>;
  terminals: Record<string, {
    identity: {
      accountKey: string;
      messageId: string;
      incarnation: string;
      payloadDigest: string;
    };
    expected: {
      accountKey: string;
      rowIncarnation: string;
      payloadDigest: string;
    };
  }>;
  smSnapshots: Record<string, {
    version: number;
    ownerGeneration: number;
    authorityEpoch: number;
    savedAt: number;
    state?: unknown;
  }>;
};

function mutableRuntimeAccount(
  store: MemoryDurableOutboundStore,
): MutableRuntimeAccountForTest {
  const accounts = (store as unknown as {
    accounts: Map<string, MutableRuntimeAccountForTest>;
  }).accounts;
  const account = accounts.get(ACCOUNT);
  if (!account) throw new Error("runtime account was not created");
  return account;
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
  test("durable decoding rejects terminal claims with mismatched row provenance", async () => {
    const store = new MemoryDurableOutboundStore();
    const owner = (await activate(store, {
      ownerId: "malformed-terminal-owner",
      ownerInstanceId: "malformed-terminal-instance",
    })).fence;
    const claimed = committedOrThrow(
      "malformed-terminal-claim",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("malformed-terminal-row"),
        createOutboundClaim(owner, 1, "sending"),
      ),
    );
    if (claimed.kind !== "claimed") throw new Error("expected claimed row");
    const recorded = committedOrThrow(
      "malformed-terminal-record",
      await store.recordTerminal(claimed.entry.identity, "ack", claimed.claim),
    );
    if (recorded.kind !== "recorded") throw new Error("expected terminal intent");

    const baseline = structuredClone(mutableRuntimeAccount(store));
    const wrongIncarnation = structuredClone(baseline);
    wrongIncarnation.terminals[recorded.intent.intentId]!.expected.rowIncarnation =
      "different-row-incarnation";
    expectCorruptRuntimeAccount(wrongIncarnation);

    const wrongDigest = structuredClone(baseline);
    wrongDigest.terminals[recorded.intent.intentId]!.expected.payloadDigest =
      "different-payload-digest";
    expectCorruptRuntimeAccount(wrongDigest);
  });

  test("same stanza id is idempotent only for the same structural payload", async () => {
    const store = new MemoryDurableOutboundStore();
    const first = committedOrThrow(
      "persist-first",
      await store.persistReady(ACCOUNT, directMessage("same-id")),
    );
    expect(first.kind).toBe("inserted");

    const duplicate = committedOrThrow(
      "persist-duplicate",
      await store.persistReady(
        ACCOUNT,
        directMessage("same-id", "hello", "2026-07-18T00:00:00.000Z"),
      ),
    );
    expect(duplicate.kind).toBe("existing");

    const collision = committedOrThrow(
      "persist-collision",
      await store.persistReady(ACCOUNT, directMessage("same-id", "different")),
    );
    expect(collision.kind).toBe("conflict");
  });

  test("claimHead atomically fences the lowest direct-lane sequence", async () => {
    const store = new MemoryDurableOutboundStore();
    const owner = (await activate(store)).fence;
    await store.persistReady(
      ACCOUNT,
      directMessage("second", "two", "2026-07-17T00:00:02.000Z"),
    );
    await store.persistReady(
      ACCOUNT,
      directMessage("first", "one", "2026-07-17T00:00:01.000Z"),
    );

    const claimed = committedOrThrow(
      "claim-first",
      await store.claimHead(
        ACCOUNT,
        { kind: "direct" },
        createOutboundClaim(owner, 1, "sending"),
      ),
    );
    expect(claimed.kind).toBe("claimed");
    if (claimed.kind !== "claimed") throw new Error("expected claim");
    expect(claimed.entry.identity.messageId).toBe("first");

    const busy = committedOrThrow(
      "claim-busy",
      await store.claimHead(
        ACCOUNT,
        { kind: "direct" },
        createOutboundClaim(owner, 1, "sending"),
      ),
    );
    expect(busy.kind).toBe("busy");
    if (busy.kind === "busy") expect(busy.messageId).toBe("first");
  });

  test("one handoff successor wins and the predecessor self-fences", async () => {
    const store = new MemoryDurableOutboundStore();
    const originalHint = {
      ownerId: "stable-owner",
      ownerInstanceId: "predecessor",
    };
    const predecessor = (await activate(store, originalHint)).fence;
    const handoff = committedOrThrow(
      "prepare",
      await store.preparePagehideHandoff(
        predecessor,
        null,
        "one-time-proof",
        smState("handoff"),
      ),
    );
    expect(handoff.kind).toBe("applied");
    if (handoff.kind !== "applied") throw new Error("expected handoff");

    const successor = await activate(store, {
      ownerId: predecessor.ownerId,
      ownerInstanceId: "successor-a",
      handoffToken: handoff.value.handoff.token,
    });
    const loser = await activate(store, {
      ownerId: predecessor.ownerId,
      ownerInstanceId: "successor-b",
      handoffToken: handoff.value.handoff.token,
    });

    expect(successor.fence.ownerId).toBe(predecessor.ownerId);
    expect(successor.fence.ownerGeneration).toBe(predecessor.ownerGeneration + 1);
    expect(successor.handoffSm?.state.previd).toBe("handoff");
    expect(successor.handoffSm?.consumed).toBe(true);
    expect(loser.fence.ownerId).not.toBe(predecessor.ownerId);
    expect(committedOrThrow(
      "renew-predecessor",
      await store.renewOwner(predecessor),
    )).toBe(false);
  });

  test("successive handoffs retain an unreconciled generation-one claim", async () => {
    const store = new MemoryDurableOutboundStore();
    const generationOne = (await activate(store, {
      ownerId: "claim-chain",
      ownerInstanceId: "claim-reused-instance-a",
    })).fence;
    const claimed = committedOrThrow(
      "claim-generation-one",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("claim-chain-row"),
        createOutboundClaim(generationOne, 1, "resume-replay"),
      ),
    );
    if (claimed.kind !== "claimed") throw new Error("expected generation-one claim");

    const generationTwo = (
      await handoffTo(store, generationOne, "claim-instance-b")
    ).fence;
    const generationThree = (
      await handoffTo(store, generationTwo, "claim-reused-instance-a")
    ).fence;

    const reconciled = committedOrThrow(
      "reconcile-generation-three",
      await store.reconcileResumeClaims(
        generationThree,
        3,
        ["claim-chain-row"],
        "resume-replay",
      ),
    );
    expect(reconciled).toMatchObject({
      kind: "reconciled",
      blockedIds: [],
      terminalIds: [],
      missingIds: [],
    });
    expect(reconciled.kind === "reconciled" && reconciled.claims[0]?.claim)
      .toMatchObject({
        ownerInstanceId: "claim-reused-instance-a",
        ownerGeneration: generationThree.ownerGeneration,
      });
  });

  test("successive handoffs retain an unapplied generation-one terminal intent", async () => {
    const store = new MemoryDurableOutboundStore();
    const generationOne = (await activate(store, {
      ownerId: "terminal-chain",
      ownerInstanceId: "terminal-reused-instance-a",
    })).fence;
    const claimed = committedOrThrow(
      "terminal-chain-claim",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("terminal-chain-row"),
        createOutboundClaim(generationOne, 1, "sending"),
      ),
    );
    if (claimed.kind !== "claimed") throw new Error("expected generation-one claim");
    const recorded = committedOrThrow(
      "terminal-chain-record",
      await store.recordTerminal(
        claimed.entry.identity,
        "ack",
        claimed.claim,
      ),
    );
    if (recorded.kind !== "recorded") throw new Error("expected terminal intent");

    const generationTwo = (
      await handoffTo(store, generationOne, "terminal-instance-b")
    ).fence;
    const generationThree = (
      await handoffTo(store, generationTwo, "terminal-reused-instance-a")
    ).fence;

    expect(committedOrThrow(
      "list-retained-terminal",
      await store.listTerminal(ACCOUNT),
    )).toEqual([recorded.intent]);
    expect(committedOrThrow(
      "apply-retained-terminal",
      await store.applyTerminal(generationThree, recorded.intent),
    ).kind).toBe("acked");
    expect(committedOrThrow(
      "rows-after-retained-terminal",
      await store.list(ACCOUNT),
    )).toEqual([]);
  });

  test("referenced predecessor saturation fails closed with typed capacity", async () => {
    const store = new MemoryDurableOutboundStore();
    let owner = (await activate(store, {
      ownerId: "capacity-chain",
      ownerInstanceId: "capacity-generation-zero",
    })).fence;

    for (let index = 0; index < RETAINED_PREDECESSOR_LIMIT; index += 1) {
      const claimed = committedOrThrow(
        `capacity-claim-${index}`,
        await store.persistClaimed(
          ACCOUNT,
          directMessage(`capacity-row-${index}`),
          createOutboundClaim(owner, index + 1, "sending"),
        ),
      );
      if (claimed.kind !== "claimed") throw new Error("expected capacity claim");
      owner = (
        await handoffTo(store, owner, `capacity-generation-${index + 1}`)
      ).fence;
    }

    const finalClaim = committedOrThrow(
      "capacity-final-claim",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("capacity-final-row"),
        createOutboundClaim(
          owner,
          RETAINED_PREDECESSOR_LIMIT + 1,
          "sending",
        ),
      ),
    );
    if (finalClaim.kind !== "claimed") throw new Error("expected final claim");
    const loaded = committedOrThrow(
      "capacity-load",
      await store.loadSm(owner),
    );
    if (loaded.kind !== "loaded") throw new Error("expected capacity owner");
    const prepared = committedOrThrow(
      "capacity-prepare",
      await store.preparePagehideHandoff(
        owner,
        loaded.version,
        "capacity-overflow-token",
        null,
      ),
    );
    if (prepared.kind !== "applied") throw new Error("expected capacity handoff");

    const outcome = await store.claimOwner(ACCOUNT, {
      ownerId: owner.ownerId,
      ownerInstanceId: "capacity-overflow-successor",
      handoffToken: prepared.value.handoff.token,
    });
    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.reason).toBe("capacity");
      expect(outcome.cause).toBeInstanceOf(DurablePredecessorCapacityError);
    }
    expect(committedOrThrow(
      "capacity-owner-still-current",
      await store.renewOwner(owner),
    )).toBe(true);
  });

  test("queued authority mutation samples time after waiting and rejects an expired owner", async () => {
    let now = 0;
    let blockNext = false;
    let releaseBlocked: (() => void) | null = null;
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
    releaseBlocked?.();

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
    const store = new MemoryDurableOutboundStore({ now: () => now });
    await activate(store, {
      ownerId: "invalid-clock-owner",
      ownerInstanceId: "invalid-clock-instance",
    });
    const before = structuredClone(mutableRuntimeAccount(store));

    now = -1;
    expectCounterExhaustion(await store.revision(ACCOUNT));

    expect(mutableRuntimeAccount(store)).toEqual(before);
  });

  test("near-limit authority time rejects lease overflow without creating an owner", async () => {
    const now = Number.MAX_SAFE_INTEGER - OUTBOUND_CLAIM_LEASE_MS + 1;
    const store = new MemoryDurableOutboundStore({ now: () => now });

    expectCounterExhaustion(await store.claimOwner(ACCOUNT, {
      ownerId: "deadline-overflow-owner",
      ownerInstanceId: "deadline-overflow-instance",
    }));

    const accounts = (store as unknown as {
      accounts: Map<string, unknown>;
    }).accounts;
    expect(accounts.has(ACCOUNT)).toBe(false);
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
    const store = new MemoryDurableOutboundStore({ now: () => now });
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
    const retentionLive = mutableRuntimeAccount(store);
    expect(retentionLive.smSnapshots[owner.ownerId]).toBeDefined();
    expect(retentionLive.owners[owner.ownerId]).toBeDefined();

    now = startedAt + SM_SNAPSHOT_RETENTION_MS + 300_000 + 1;
    committedOrThrow(
      "retention-expired-scan",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    const retentionExpired = mutableRuntimeAccount(store);
    expect(retentionExpired.smSnapshots[owner.ownerId]).toBeUndefined();
    expect(retentionExpired.owners[owner.ownerId]).toBeUndefined();
  });

  test("SM tombstones retain their exact owner until the tombstone TTL expires", async () => {
    const startedAt = 2_000;
    let now = startedAt;
    const store = new MemoryDurableOutboundStore({ now: () => now });
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
    const retained = mutableRuntimeAccount(store);
    expect(retained.smSnapshots[owner.ownerId]?.state).toBeNull();
    expect(retained.owners[owner.ownerId]).toBeDefined();

    now = startedAt + SM_SNAPSHOT_RETENTION_MS + 1;
    committedOrThrow(
      "tombstone-expired-scan",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    const expired = mutableRuntimeAccount(store);
    expect(expired.smSnapshots[owner.ownerId]).toBeUndefined();
    expect(expired.owners[owner.ownerId]).toBeUndefined();
  });

  test("renewed exact owners protect retention-expired SM state and tombstones only until lease expiry", async () => {
    // A resumable snapshot is retained for its advertised 300-second resume
    // window plus the eight-day terminal-reference horizon. Start beyond that
    // combined deadline so only the exact renewed owner can protect either
    // record in the first scan.
    let now = SM_SNAPSHOT_RETENTION_MS + 300_000 + 10_000;
    const store = new MemoryDurableOutboundStore({ now: () => now });
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

    const seeded = mutableRuntimeAccount(store);
    seeded.smSnapshots[snapshotOwner.ownerId]!.savedAt = 0;
    seeded.smSnapshots[tombstoneOwner.ownerId]!.savedAt = 0;
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
    const protectedRecords = mutableRuntimeAccount(store).smSnapshots;
    expect(protectedRecords[snapshotOwner.ownerId]?.state).toBeDefined();
    expect(protectedRecords[tombstoneOwner.ownerId]?.state).toBeNull();

    now += OUTBOUND_CLAIM_LEASE_MS + 1;
    committedOrThrow(
      "scan-expired-old-sm",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    const expiredRecords = mutableRuntimeAccount(store).smSnapshots;
    expect(expiredRecords[snapshotOwner.ownerId]).toBeUndefined();
    expect(expiredRecords[tombstoneOwner.ownerId]).toBeUndefined();
  });

  test("foreign live claim blocks native resume without partial adoption", async () => {
    const store = new MemoryDurableOutboundStore();
    const ownerA = (await activate(store, {
      ownerId: "sender-a",
      ownerInstanceId: "sender-a-instance",
    })).fence;
    const ownerB = (await activate(store, {
      ownerId: "sender-b",
      ownerInstanceId: "sender-b-instance",
    })).fence;
    const claimed = committedOrThrow(
      "claim-a",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("foreign-live"),
        createOutboundClaim(ownerA, 1, "sending"),
      ),
    );
    if (claimed.kind !== "claimed") throw new Error("expected claimed row");

    const reconciliation = committedOrThrow(
      "reconcile-b",
      await store.reconcileResumeClaims(
        ownerB,
        2,
        ["foreign-live"],
        "resume-replay",
      ),
    );
    expect(reconciliation).toMatchObject({
      kind: "reconciled",
      claims: [],
      blockedIds: ["foreign-live"],
    });

    const scan = committedOrThrow(
      "scan",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    const state = scan.entries[0]?.state;
    expect(state?.kind).toBe("claimed");
    if (state?.kind === "claimed") {
      expect(state.claim.ownerId).toBe(ownerA.ownerId);
      expect(state.claim.claimId).toBe(claimed.claim.claimId);
    }
  });

  test("resume reconciliation preflights the full snapshot before adopting any row", async () => {
    const store = new MemoryDurableOutboundStore();
    const ownerA = (await activate(store, {
      ownerId: "preflight-a",
      ownerInstanceId: "preflight-a-instance",
    })).fence;
    const ownerB = (await activate(store, {
      ownerId: "preflight-b",
      ownerInstanceId: "preflight-b-instance",
    })).fence;
    await store.persistReady(
      ACCOUNT,
      directMessage("adoptable", "first", "2026-07-17T00:00:00.000Z"),
    );
    const foreign = committedOrThrow(
      "foreign-claim",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("blocked", "second", "2026-07-17T00:00:01.000Z"),
        createOutboundClaim(ownerA, 1, "sending"),
      ),
    );
    if (foreign.kind !== "claimed") throw new Error("expected foreign claim");

    const reconciliation = committedOrThrow(
      "preflight-reconcile",
      await store.reconcileResumeClaims(
        ownerB,
        2,
        ["adoptable", "blocked"],
        "resume-replay",
      ),
    );
    expect(reconciliation).toMatchObject({
      kind: "reconciled",
      claims: [],
      releasedIds: [],
      blockedIds: ["blocked"],
    });

    const scan = committedOrThrow(
      "preflight-scan",
      await store.scanAndPrune(ACCOUNT, Number.MIN_SAFE_INTEGER),
    );
    expect(scan.entries.map((entry) => entry.state.kind)).toEqual([
      "ready",
      "claimed",
    ]);
    const blocked = scan.entries[1]?.state;
    if (blocked?.kind !== "claimed") throw new Error("expected blocked claim");
    expect(blocked.claim.claimId).toBe(foreign.claim.claimId);
  });

  test("terminal intent marks the row before atomic acknowledgement apply", async () => {
    const store = new MemoryDurableOutboundStore();
    const owner = (await activate(store)).fence;
    const persisted = committedOrThrow(
      "persist",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("terminal"),
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
    if (terminal.kind !== "recorded") throw new Error("expected terminal intent");

    const blocked = committedOrThrow(
      "terminal-head",
      await store.claimHead(
        ACCOUNT,
        { kind: "direct" },
        createOutboundClaim(owner, 1, "sending"),
      ),
    );
    expect(blocked.kind).toBe("terminal");
    expect((await store.listTerminal(ACCOUNT)).kind).toBe("committed");

    const wrongIncarnation = structuredClone(terminal.intent);
    wrongIncarnation.expected.rowIncarnation = "foreign-incarnation";
    expect(committedOrThrow(
      "apply-terminal-wrong-incarnation",
      await store.applyTerminal(owner, wrongIncarnation),
    ).kind).toBe("missing");
    const wrongDigest = structuredClone(terminal.intent);
    wrongDigest.expected.payloadDigest = "foreign-digest";
    expect(committedOrThrow(
      "apply-terminal-wrong-digest",
      await store.applyTerminal(owner, wrongDigest),
    ).kind).toBe("missing");
    expect(committedOrThrow(
      "terminal-intact-after-provenance-rejection",
      await store.listTerminal(ACCOUNT),
    )).toEqual([terminal.intent]);
    expect(committedOrThrow(
      "row-intact-after-provenance-rejection",
      await store.list(ACCOUNT),
    )).toHaveLength(1);

    const applied = committedOrThrow(
      "apply-terminal",
      await store.applyTerminal(owner, terminal.intent),
    );
    expect(applied.kind).toBe("acked");
    expect(committedOrThrow("rows", await store.list(ACCOUNT))).toEqual([]);
    expect(committedOrThrow("intents", await store.listTerminal(ACCOUNT))).toEqual([]);
  });

  test("only the current executor may apply a predecessor's recorded terminal intent", async () => {
    const store = new MemoryDurableOutboundStore();
    const predecessor = (await activate(store, {
      ownerId: "terminal-handoff",
      ownerInstanceId: "terminal-predecessor",
    })).fence;
    const persisted = committedOrThrow(
      "terminal-handoff-persist",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("terminal-handoff-row"),
        createOutboundClaim(predecessor, 1, "sending"),
      ),
    );
    if (persisted.kind !== "claimed") throw new Error("expected claimed row");
    const terminal = committedOrThrow(
      "terminal-handoff-record",
      await store.recordTerminal(
        persisted.entry.identity,
        "ack",
        persisted.claim,
      ),
    );
    if (terminal.kind !== "recorded") throw new Error("expected terminal intent");
    const prepared = committedOrThrow(
      "terminal-handoff-prepare",
      await store.preparePagehideHandoff(
        predecessor,
        null,
        "terminal-handoff-proof",
        null,
      ),
    );
    if (prepared.kind !== "applied") throw new Error("expected handoff");
    const successor = await activate(store, {
      ownerId: predecessor.ownerId,
      ownerInstanceId: "terminal-successor",
      handoffToken: prepared.value.handoff.token,
    });

    const staleExecutor = committedOrThrow(
      "terminal-predecessor-apply",
      await store.applyTerminal(predecessor, terminal.intent),
    );
    expect(staleExecutor.kind).toBe("fenced");
    expect(committedOrThrow(
      "terminal-still-recorded",
      await store.listTerminal(ACCOUNT),
    )).toHaveLength(1);

    const applied = committedOrThrow(
      "terminal-successor-apply",
      await store.applyTerminal(successor.fence, terminal.intent),
    );
    expect(applied.kind).toBe("acked");
    expect(committedOrThrow(
      "terminal-handoff-rows",
      await store.list(ACCOUNT),
    )).toEqual([]);
  });

  test("pagehide atomically extends the owner lease through the handoff deadline", async () => {
    let now = 0;
    const store = new MemoryDurableOutboundStore({ now: () => now });
    const predecessor = (await activate(store, {
      ownerId: "lease-handoff",
      ownerInstanceId: "lease-predecessor",
    })).fence;
    now = 44_000;
    const prepared = committedOrThrow(
      "lease-handoff-prepare",
      await store.preparePagehideHandoff(
        predecessor,
        null,
        "lease-handoff-proof",
        smState("lease-handoff"),
      ),
    );
    if (prepared.kind !== "applied") throw new Error("expected handoff");

    now = 46_000;
    const successor = await activate(store, {
      ownerId: predecessor.ownerId,
      ownerInstanceId: "lease-successor",
      handoffToken: prepared.value.handoff.token,
    });
    expect(successor.fence.ownerId).toBe(predecessor.ownerId);
    expect(successor.fence.ownerGeneration).toBe(predecessor.ownerGeneration + 1);
    expect(successor.handoffSm?.state.previd).toBe("lease-handoff");
  });

  test("a null-state handoff transfers the SM tombstone fence to its successor", async () => {
    const store = new MemoryDurableOutboundStore();
    const predecessor = (await activate(store, {
      ownerId: "null-handoff",
      ownerInstanceId: "null-predecessor",
    })).fence;
    const prepared = committedOrThrow(
      "prepare-null-handoff",
      await store.preparePagehideHandoff(
        predecessor,
        null,
        "null-handoff-proof",
        null,
      ),
    );
    if (prepared.kind !== "applied") throw new Error("expected handoff");

    const successor = await activate(store, {
      ownerId: predecessor.ownerId,
      ownerInstanceId: "null-successor",
      handoffToken: prepared.value.handoff.token,
    });
    expect(successor.handoffSm).toBeUndefined();
    const loaded = committedOrThrow(
      "load-transferred-tombstone",
      await store.loadSm(successor.fence),
    );
    expect(loaded.kind === "loaded" && loaded.envelope).toBeNull();
    expect(loaded.kind === "loaded" && loaded.version).toBe(
      prepared.value.smVersion + 1,
    );
  });

  test("handoff cancellation is exact-token and exact-SM-version fenced", async () => {
    const store = new MemoryDurableOutboundStore();
    const owner = (await activate(store, {
      ownerId: "cancel-handoff",
      ownerInstanceId: "cancel-owner",
    })).fence;
    const prepared = committedOrThrow(
      "cancel-prepare",
      await store.preparePagehideHandoff(
        owner,
        null,
        "cancel-exact-token",
        smState("cancel-state"),
      ),
    );
    if (prepared.kind !== "applied") throw new Error("expected handoff");
    const revision = committedOrThrow(
      "cancel-revision-before-stale",
      await store.revision(ACCOUNT),
    );

    const wrongToken = committedOrThrow(
      "cancel-wrong-token",
      await store.cancelOwnerHandoff(
        owner,
        "cancel-wrong-token",
        prepared.value.smVersion,
      ),
    );
    expect(wrongToken.kind).toBe("stale");
    const staleVersion = committedOrThrow(
      "cancel-stale-version",
      await store.cancelOwnerHandoff(
        owner,
        prepared.value.handoff.token,
        prepared.value.smVersion - 1,
      ),
    );
    expect(staleVersion.kind).toBe("stale");
    expect(committedOrThrow(
      "cancel-revision-after-stale",
      await store.revision(ACCOUNT),
    )).toBe(revision);

    expect(committedOrThrow(
      "cancel-exact",
      await store.cancelOwnerHandoff(
        owner,
        prepared.value.handoff.token,
        prepared.value.smVersion,
      ),
    )).toEqual({ kind: "applied", cancelled: true });

    const invalidSuccessor = await activate(store, {
      ownerId: owner.ownerId,
      ownerInstanceId: "cancel-invalid-successor",
      handoffToken: prepared.value.handoff.token,
    });
    expect(invalidSuccessor.fence.ownerId).not.toBe(owner.ownerId);
  });

  test("handoff cancellation is fenced after a successor consumes it", async () => {
    const store = new MemoryDurableOutboundStore();
    const predecessor = (await activate(store, {
      ownerId: "consumed-cancel",
      ownerInstanceId: "consumed-predecessor",
    })).fence;
    const prepared = committedOrThrow(
      "consumed-cancel-prepare",
      await store.preparePagehideHandoff(
        predecessor,
        null,
        "consumed-cancel-token",
        smState("consumed-cancel-state"),
      ),
    );
    if (prepared.kind !== "applied") throw new Error("expected handoff");
    const successor = await activate(store, {
      ownerId: predecessor.ownerId,
      ownerInstanceId: "consumed-successor",
      handoffToken: prepared.value.handoff.token,
    });
    expect(successor.fence.ownerGeneration).toBe(
      predecessor.ownerGeneration + 1,
    );

    expect(committedOrThrow(
      "cancel-after-consumption",
      await store.cancelOwnerHandoff(
        predecessor,
        prepared.value.handoff.token,
        prepared.value.smVersion,
      ),
    ).kind).toBe("fenced");
  });

  test("retired fallback generation releases once and rejects late callbacks", async () => {
    const store = new MemoryDurableOutboundStore();
    const owner = (await activate(store)).fence;
    const persisted = committedOrThrow(
      "persist",
      await store.persistClaimed(
        ACCOUNT,
        directMessage("fallback"),
        createOutboundClaim(owner, 1, "resume-replay"),
      ),
    );
    if (persisted.kind !== "claimed") throw new Error("expected claimed row");

    const failure = committedOrThrow(
      "record-failure",
      await store.recordTerminal(
        persisted.entry.identity,
        "native-failure",
        persisted.claim,
      ),
    );
    if (failure.kind !== "recorded") throw new Error("expected failure intent");
    const fallback = committedOrThrow(
      "apply-failure",
      await store.applyTerminal(owner, failure.intent),
    );
    if (fallback.kind !== "fallback") throw new Error("expected fallback");

    const sameGeneration = committedOrThrow(
      "same-generation",
      await store.reconcileResumeClaims(
        owner,
        1,
        ["fallback"],
        "fresh-fallback",
      ),
    );
    if (sameGeneration.kind !== "reconciled") throw new Error("expected reconciliation");
    expect(sameGeneration.claims[0]?.claim.claimId).toBe(fallback.claim.claimId);

    const retired = committedOrThrow(
      "retire",
      await store.reconcileResumeClaims(owner, 2, null, "resume-replay"),
    );
    if (retired.kind !== "reconciled") throw new Error("expected retirement");
    expect(retired.releasedIds).toEqual(["fallback"]);

    const late = committedOrThrow(
      "late-ack",
      await store.recordTerminal(
        persisted.entry.identity,
        "ack",
        fallback.claim,
      ),
    );
    expect(late.kind).toBe("stale");

    const retry = committedOrThrow(
      "retry",
      await store.claimHead(
        ACCOUNT,
        { kind: "direct" },
        createOutboundClaim(owner, 2, "sending"),
      ),
    );
    expect(retry.kind).toBe("claimed");
  });

  test("SM snapshots are owner-scoped, version-fenced, and tombstoned", async () => {
    const store = new MemoryDurableOutboundStore();
    const ownerA = (await activate(store, {
      ownerId: "owner-a",
      ownerInstanceId: "instance-a",
    })).fence;
    const ownerB = (await activate(store, {
      ownerId: "owner-b",
      ownerInstanceId: "instance-b",
    })).fence;

    const savedA = committedOrThrow(
      "save-a",
      await store.saveSm(ownerA, null, smState("a"), 100),
    );
    const savedB = committedOrThrow(
      "save-b",
      await store.saveSm(ownerB, null, smState("b"), 200),
    );
    expect(savedA.kind).toBe("applied");
    expect(savedB.kind).toBe("applied");

    const loadedA = committedOrThrow("load-a", await store.loadSm(ownerA));
    const loadedB = committedOrThrow("load-b", await store.loadSm(ownerB));
    expect(loadedA.kind === "loaded" && loadedA.envelope?.state.previd).toBe("a");
    expect(loadedB.kind === "loaded" && loadedB.envelope?.state.previd).toBe("b");
    if (savedA.kind !== "applied") throw new Error("expected save");

    const cleared = committedOrThrow(
      "clear-a",
      await store.clearSm(ownerA, savedA.value.version),
    );
    if (cleared.kind !== "applied") throw new Error("expected clear");
    const stale = committedOrThrow(
      "stale-save",
      await store.saveSm(ownerA, savedA.value.version, smState("stale"), 300),
    );
    expect(stale.kind).toBe("stale");

    const afterClear = committedOrThrow("load-cleared", await store.loadSm(ownerA));
    expect(afterClear.kind === "loaded" && afterClear.envelope).toBeNull();
    expect(afterClear.kind === "loaded" && afterClear.version).toBe(cleared.value.version);
  });

  test("SM semantic validation fails before mutation and preserves revision", async () => {
    const store = new MemoryDurableOutboundStore();
    const owner = (await activate(store)).fence;
    const revision = committedOrThrow(
      "revision-before-invalid-sm",
      await store.revision(ACCOUNT),
    );
    const invalid = {
      ...smState("invalid"),
      inboundH: 0x1_0000_0000,
    } as PersistedSmResumeState;

    const outcome = await store.saveSm(owner, null, invalid, 100);

    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.cause).toBeInstanceOf(DOMException);
      expect((outcome.cause as DOMException).name).toBe("DataError");
    }
    expect(committedOrThrow(
      "revision-after-invalid-sm",
      await store.revision(ACCOUNT),
    )).toBe(revision);
    const loaded = committedOrThrow(
      "load-after-invalid-sm",
      await store.loadSm(owner),
    );
    expect(loaded.kind === "loaded" && loaded.envelope).toBeNull();
  });

  test("account revision and every SM version mutation fail closed at MAX_SAFE_INTEGER", async () => {
    const revisionStore = new MemoryDurableOutboundStore();
    await revisionStore.persistReady(ACCOUNT, directMessage("revision-seed"));
    const revisionAccount = mutableRuntimeAccount(revisionStore);
    revisionAccount.revision = Number.MAX_SAFE_INTEGER;
    expectCounterExhaustion(
      await revisionStore.persistReady(
        ACCOUNT,
        directMessage("revision-overflow"),
      ),
    );
    expect(revisionAccount.revision).toBe(Number.MAX_SAFE_INTEGER);

    const smStore = new MemoryDurableOutboundStore();
    const owner = (await activate(smStore)).fence;
    const saved = committedOrThrow(
      "seed-max-sm-version",
      await smStore.saveSm(owner, null, smState("max-version"), 100),
    );
    if (saved.kind !== "applied") throw new Error("expected SM seed");
    mutableRuntimeAccount(smStore).smSnapshots[owner.ownerId]!.version =
      Number.MAX_SAFE_INTEGER;

    expectCounterExhaustion(
      await smStore.saveSm(
        owner,
        Number.MAX_SAFE_INTEGER,
        smState("save-overflow"),
        200,
      ),
    );
    expectCounterExhaustion(
      await smStore.consumeSm(
        owner,
        Number.MAX_SAFE_INTEGER,
        () => true,
      ),
    );
    expectCounterExhaustion(
      await smStore.clearSm(owner, Number.MAX_SAFE_INTEGER),
    );
    expectCounterExhaustion(
      await smStore.preparePagehideHandoff(
        owner,
        Number.MAX_SAFE_INTEGER,
        "max-version-handoff",
        smState("handoff-overflow"),
      ),
    );

    const loaded = committedOrThrow(
      "load-max-sm-version",
      await smStore.loadSm(owner),
    );
    expect(loaded.kind === "loaded" && loaded.version).toBe(
      Number.MAX_SAFE_INTEGER,
    );
    expect(
      loaded.kind === "loaded" && loaded.envelope?.state.previd,
    ).toBe("max-version");
  });

  test("owner-generation and authority-epoch exhaustion fence without partial commit", async () => {
    const generationStore = new MemoryDurableOutboundStore({
      now: () => 50_000,
    });
    await activate(generationStore, {
      ownerId: "generation-seed",
      ownerInstanceId: "generation-seed-instance",
    });
    const generationAccount = mutableRuntimeAccount(generationStore);
    generationAccount.nextOwnerGeneration = Number.MAX_SAFE_INTEGER;
    expectCounterExhaustion(await generationStore.claimOwner(ACCOUNT, {
      ownerId: "generation-overflow",
      ownerInstanceId: "generation-overflow-instance",
    }));
    expect(generationAccount.nextOwnerGeneration).toBe(
      Number.MAX_SAFE_INTEGER,
    );

    let now = 50_000;
    const epochStore = new MemoryDurableOutboundStore({ now: () => now });
    await epochStore.revision(ACCOUNT);
    const epochAccount = mutableRuntimeAccount(epochStore);
    epochAccount.authorityEpoch = Number.MAX_SAFE_INTEGER;
    epochAccount.lastWallClockSampleMs = now;
    now = 1_000;
    expectCounterExhaustion(await epochStore.revision(ACCOUNT));
    expect(epochAccount.authorityEpoch).toBe(Number.MAX_SAFE_INTEGER);
  });
});
