import { describe, expect, test } from "bun:test";
import {
  committedOrThrow,
  createOutboundClaim,
  type DurableOutboundStore,
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
    const {
      store: revisionStore,
      repository: revisionRepository,
    } = recordingDurableStore({ now: () => Date.now() });
    await revisionStore.persistReady(ACCOUNT, directMessage("revision-seed"));
    revisionRepository.mutate(ACCOUNT, (revisionAccount) => {
      revisionAccount.revision = Number.MAX_SAFE_INTEGER;
    });
    expectCounterExhaustion(
      await revisionStore.persistReady(
        ACCOUNT,
        directMessage("revision-overflow"),
      ),
    );
    expect(revisionRepository.inspect(ACCOUNT).revision).toBe(
      Number.MAX_SAFE_INTEGER,
    );

    const {
      store: smStore,
      repository: smRepository,
    } = recordingDurableStore({ now: () => Date.now() });
    const owner = (await activate(smStore)).fence;
    const saved = committedOrThrow(
      "seed-max-sm-version",
      await smStore.saveSm(owner, null, smState("max-version"), 100),
    );
    if (saved.kind !== "applied") throw new Error("expected SM seed");
    smRepository.mutate(ACCOUNT, (account) => {
      account.smSnapshots[owner.ownerId]!.version = Number.MAX_SAFE_INTEGER;
    });

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
    const {
      store: generationStore,
      repository: generationRepository,
    } = recordingDurableStore({
      now: () => 50_000,
    });
    await activate(generationStore, {
      ownerId: "generation-seed",
      ownerInstanceId: "generation-seed-instance",
    });
    generationRepository.mutate(ACCOUNT, (generationAccount) => {
      generationAccount.nextOwnerGeneration = Number.MAX_SAFE_INTEGER;
    });
    expectCounterExhaustion(await generationStore.claimOwner(ACCOUNT, {
      ownerId: "generation-overflow",
      ownerInstanceId: "generation-overflow-instance",
    }));
    expect(generationRepository.inspect(ACCOUNT).nextOwnerGeneration).toBe(
      Number.MAX_SAFE_INTEGER,
    );

    let now = 50_000;
    const {
      store: epochStore,
      repository: epochRepository,
    } = recordingDurableStore({ now: () => now });
    await epochStore.revision(ACCOUNT);
    epochRepository.mutate(ACCOUNT, (epochAccount) => {
      epochAccount.authorityEpoch = Number.MAX_SAFE_INTEGER;
      epochAccount.lastWallClockSampleMs = now;
    });
    now = 1_000;
    expectCounterExhaustion(await epochStore.revision(ACCOUNT));
    expect(epochRepository.inspect(ACCOUNT).authorityEpoch).toBe(
      Number.MAX_SAFE_INTEGER,
    );
  });
});
