import { describe, expect, test } from "bun:test";
import {
  DurablePredecessorCapacityError,
  RETAINED_PREDECESSOR_LIMIT,
  type OutboundClaim,
  type OutboundOwnerContext,
} from "../src/lib/xmpp-runtime/durable-contract";
import {
  emptyAccount,
  type RuntimeAccount,
} from "../src/lib/xmpp-runtime/durable-model";
import {
  cancelOwnerHandoffTransition,
  claimOwnerTransition,
  clearSmTransition,
  consumeSmTransition,
  loadSmTransition,
  preparePagehideHandoffTransition,
  renewOwnerTransition,
  saveSmTransition,
} from "../src/lib/xmpp-runtime/durable-owner-sm-transitions";
import type { PersistedSmResumeState } from "../src/lib/xmpp/sm-resume-types";

const ACCOUNT = "owner-sm-transitions@example.com";
const NOW = 1_000;

function smState(previd = "resume"): PersistedSmResumeState {
  return {
    previd,
    inboundH: 4,
    outboundH: 7,
    maxResumeSeconds: 300,
    unhandledOutboundEntries: [],
  };
}

function activate(
  account: RuntimeAccount,
  ownerId = "owner",
  ownerInstanceId = "instance",
  rotatedOwnerId = "rotated-owner",
  authorityNow = NOW,
): OutboundOwnerContext {
  return claimOwnerTransition(
    account,
    authorityNow,
    { ownerId, ownerInstanceId },
    rotatedOwnerId,
  ).value.fence;
}

function claimedRow(
  owner: OutboundOwnerContext,
  id: string,
  claim: Partial<OutboundClaim> = {},
) {
  const incarnation = `${id}-incarnation`;
  const payloadDigest = `${id}-digest`;
  const message = {
    kind: "dm" as const,
    id,
    createdAt: "2026-07-17T00:00:00.000Z",
    peerJid: "recipient@example.com",
    body: id,
  };
  return {
    identity: {
      accountKey: ACCOUNT,
      messageId: id,
      incarnation,
      payloadDigest,
    },
    lane: { kind: "direct" as const },
    orderKey: `${message.createdAt}\u0000${id}`,
    message,
    state: {
      kind: "claimed" as const,
      claim: {
        ...owner,
        connectionGeneration: 1,
        claimId: `${id}-claim`,
        phase: "sending" as const,
        rowIncarnation: incarnation,
        payloadDigest,
        leaseUntil: 100_000,
        ...claim,
      },
    },
  };
}

describe("durable owner and SM transitions", () => {
  test("claims, renews, and rotates owner authority without reviving history", () => {
    const account = emptyAccount(ACCOUNT);
    const first = activate(account);
    expect(first).toMatchObject({
      accountKey: ACCOUNT,
      ownerId: "owner",
      ownerInstanceId: "instance",
      ownerGeneration: 1,
      authorityEpoch: 0,
    });

    const sameInstance = claimOwnerTransition(
      account,
      NOW + 1,
      { ownerId: "owner", ownerInstanceId: "instance" },
      "unused-rotation",
    );
    expect(sameInstance.value.fence.ownerGeneration).toBe(1);
    expect(account.owners.owner?.lastRenewedAt).toBe(NOW + 1);

    expect(renewOwnerTransition(
      account,
      NOW + 2,
      sameInstance.value.fence,
    )).toMatchObject({ changed: true, value: true });

    const expiredAt = account.owners.owner!.leaseUntil;
    const rotated = claimOwnerTransition(
      account,
      expiredAt,
      { ownerId: "owner", ownerInstanceId: "instance" },
      "rotated-after-expiry",
    );
    expect(rotated.value.fence).toMatchObject({
      ownerId: "rotated-after-expiry",
      ownerInstanceId: "instance",
      ownerGeneration: 2,
    });
    expect(account.owners.owner?.ownerGeneration).toBe(1);
    expect(renewOwnerTransition(
      account,
      expiredAt,
      first,
    )).toEqual({ changed: false, value: false });
  });

  test("transfers one exact handoff and consumes its SM snapshot", () => {
    const account = emptyAccount(ACCOUNT);
    const predecessor = activate(account);
    const prepared = preparePagehideHandoffTransition(
      account,
      NOW + 1,
      predecessor,
      null,
      "handoff-token",
      smState(),
    );
    expect(prepared.value.kind).toBe("applied");
    if (prepared.value.kind !== "applied") throw new Error("expected handoff");

    const successor = claimOwnerTransition(
      account,
      NOW + 2,
      {
        ownerId: predecessor.ownerId,
        ownerInstanceId: "successor-instance",
        handoffToken: "handoff-token",
      },
      "unused-rotation",
    );
    expect(successor.value.fence).toMatchObject({
      ownerId: predecessor.ownerId,
      ownerInstanceId: "successor-instance",
      ownerGeneration: 2,
    });
    expect(successor.value.handoffSm).toMatchObject({
      ownerGeneration: 2,
      version: 2,
      consumed: true,
      state: { previd: "resume" },
    });
    expect(account.smSnapshots[predecessor.ownerId]).toMatchObject({
      ownerGeneration: 2,
      version: 2,
      consumed: true,
    });
  });

  test("fails closed when a handoff would exceed predecessor capacity", () => {
    const account = emptyAccount(ACCOUNT);
    const owner: OutboundOwnerContext = {
      accountKey: ACCOUNT,
      ownerId: "owner",
      ownerInstanceId: "current-instance",
      ownerGeneration: RETAINED_PREDECESSOR_LIMIT + 1,
      authorityEpoch: 0,
    };
    account.nextOwnerGeneration = RETAINED_PREDECESSOR_LIMIT + 2;
    account.owners.owner = {
      ownerId: owner.ownerId,
      ownerInstanceId: owner.ownerInstanceId,
      ownerGeneration: owner.ownerGeneration,
      authorityEpoch: owner.authorityEpoch,
      leaseUntil: 100_000,
      lastRenewedAt: NOW,
      handoff: {
        token: "capacity-handoff",
        expiresAt: 100_000,
        authorityEpoch: owner.authorityEpoch,
        ownerGeneration: owner.ownerGeneration,
      },
      predecessors: Array.from(
        { length: RETAINED_PREDECESSOR_LIMIT },
        (_, index) => ({
          ownerInstanceId: `predecessor-${index + 1}`,
          ownerGeneration: index + 1,
          authorityEpoch: 0,
          expiresAt: 100_000,
        }),
      ),
    };
    for (let index = 1; index <= RETAINED_PREDECESSOR_LIMIT; index += 1) {
      const predecessor = {
        ...owner,
        ownerInstanceId: `predecessor-${index}`,
        ownerGeneration: index,
      };
      account.outbound[`predecessor-row-${index}`] = claimedRow(
        predecessor,
        `predecessor-row-${index}`,
      );
    }
    account.outbound.current = claimedRow(owner, "current");

    expect(() => claimOwnerTransition(
      account,
      NOW + 1,
      {
        ownerId: owner.ownerId,
        ownerInstanceId: "successor-instance",
        handoffToken: "capacity-handoff",
      },
      "unused-rotation",
    )).toThrow(DurablePredecessorCapacityError);
    expect(account.nextOwnerGeneration).toBe(RETAINED_PREDECESSOR_LIMIT + 2);
  });

  test("loads, saves, consumes, and clears with exact CAS and owner fences", () => {
    const account = emptyAccount(ACCOUNT);
    const owner = activate(account);

    expect(loadSmTransition(account, NOW, owner).value).toEqual({
      kind: "loaded",
      envelope: null,
      version: null,
    });

    const saved = saveSmTransition(
      account,
      NOW + 1,
      owner,
      null,
      smState(),
    );
    expect(saved.value.kind).toBe("applied");
    if (saved.value.kind !== "applied") throw new Error("expected saved SM");
    expect(saved.value.value.version).toBe(1);
    saved.value.value.state.inboundH = 99;
    expect(account.smSnapshots.owner?.state?.inboundH).toBe(4);

    const staleSave = saveSmTransition(
      account,
      NOW + 2,
      owner,
      null,
      smState("stale"),
    );
    expect(staleSave).toEqual({
      changed: false,
      value: { kind: "stale", actualVersion: 1 },
    });

    expect(loadSmTransition(account, NOW + 2, owner).value).toMatchObject({
      kind: "loaded",
      version: 1,
      envelope: { consumed: false, state: { previd: "resume" } },
    });

    expect(consumeSmTransition(
      account,
      NOW + 2,
      owner,
      0,
      () => true,
    ).value).toEqual({ kind: "stale", actualVersion: 1 });

    const consumed = consumeSmTransition(
      account,
      NOW + 3,
      owner,
      1,
      (envelope) => envelope.state.previd === "resume",
    );
    expect(consumed.value).toMatchObject({
      kind: "applied",
      value: { version: 2, consumed: true },
    });
    expect(consumeSmTransition(
      account,
      NOW + 4,
      owner,
      2,
      () => true,
    )).toEqual({
      changed: false,
      value: { kind: "applied", value: null },
    });

    expect(clearSmTransition(
      account,
      NOW + 5,
      owner,
      1,
    ).value).toEqual({ kind: "stale", actualVersion: 2 });
    const cleared = clearSmTransition(
      account,
      NOW + 5,
      owner,
      2,
    );
    expect(cleared.value).toEqual({
      kind: "applied",
      value: { cleared: true, version: 3 },
    });
    expect(loadSmTransition(account, NOW + 6, owner).value).toEqual({
      kind: "loaded",
      envelope: null,
      version: 3,
    });

    const fencedOwner = {
      ...owner,
      ownerGeneration: owner.ownerGeneration + 1,
    };
    expect(loadSmTransition(
      account,
      NOW + 6,
      fencedOwner,
    ).value.kind).toBe("fenced");
    expect(clearSmTransition(
      account,
      NOW + 6,
      fencedOwner,
      3,
    ).value.kind).toBe("fenced");
  });

  test("cancels handoff only for the exact token and SM version", () => {
    const account = emptyAccount(ACCOUNT);
    const owner = activate(account);
    const prepared = preparePagehideHandoffTransition(
      account,
      NOW + 1,
      owner,
      null,
      "exact-token",
      null,
    );
    if (prepared.value.kind !== "applied") throw new Error("expected handoff");
    const version = prepared.value.value.smVersion;

    expect(cancelOwnerHandoffTransition(
      account,
      NOW + 2,
      owner,
      "wrong-token",
      version,
    ).value).toMatchObject({
      kind: "stale",
      actualToken: "exact-token",
      actualSmVersion: version,
    });
    expect(cancelOwnerHandoffTransition(
      account,
      NOW + 2,
      owner,
      "exact-token",
      version,
    )).toEqual({
      changed: true,
      value: { kind: "applied", cancelled: true },
    });
  });
});
