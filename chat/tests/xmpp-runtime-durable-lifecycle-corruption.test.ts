import { describe, expect, test } from "bun:test";
import {
  DurablePredecessorCapacityError,
  RETAINED_PREDECESSOR_LIMIT,
  committedOrThrow,
  createOutboundClaim,
  type DurableOutboundStore,
  type OutboundOwnerActivation,
  type OutboundOwnerHint,
} from "../src/lib/xmpp-runtime/durable-contract";
import { validatePersistedRuntimeAccount } from "../src/lib/xmpp-runtime/durable-codec";
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

async function handoffTo(
  store: DurableOutboundStore,
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

describe("unified XMPP runtime authority", () => {
  test("durable decoding rejects terminal claims with mismatched row provenance", async () => {
    const { store, repository } = recordingDurableStore({
      now: () => Date.now(),
    });
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

    const baseline = repository.inspect(ACCOUNT);
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

});
