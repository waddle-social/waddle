import { describe, expect, test } from "bun:test";
import type {
  PersistedQueuedDmMessage,
  PersistedQueuedRoomMessage,
} from "../src/lib/outbound-queue-store";
import {
  outboundLane,
  type OutboundClaimRequest,
  type OutboundOwnerContext,
} from "../src/lib/xmpp-runtime/durable-contract";
import type { RuntimeAccount } from "../src/lib/xmpp-runtime/durable-model";
import {
  claimForRow,
  emptyAccount,
  entryFromRow,
} from "../src/lib/xmpp-runtime/durable-model";
import {
  applyTerminalTransition,
  claimHeadTransition,
  listOutboundTransition,
  persistAndClaimLaneHeadTransition,
  persistReadyTransition,
  reconcileResumeClaimsTransition,
  recordTerminalTransition,
  releaseClaimTransition,
  releaseForFreshSessionTransition,
  renewClaimTransition,
  revisionTransition,
  scanAndPruneTransition,
  type PreparedOutboundMessage,
} from "../src/lib/xmpp-runtime/durable-outbound-transitions";
import {
  claimOwnerTransition,
  preparePagehideHandoffTransition,
} from "../src/lib/xmpp-runtime/durable-owner-sm-transitions";

const ACCOUNT = "outbound-transitions@example.com";
const NOW = 1_000;

function directMessage(
  id: string,
  body = id,
  createdAt = "2026-07-17T00:00:00.000Z",
): PersistedQueuedDmMessage {
  return {
    kind: "dm",
    id,
    createdAt,
    peerJid: "recipient@example.com",
    body,
  };
}

function prepared(
  id: string,
  options: {
    body?: string;
    createdAt?: string;
    digest?: string;
    incarnation?: string;
  } = {},
): PreparedOutboundMessage {
  const message = directMessage(
    id,
    options.body,
    options.createdAt,
  );
  return {
    identity: {
      accountKey: ACCOUNT,
      messageId: id,
      incarnation: options.incarnation ?? `${id}-incarnation`,
      payloadDigest: options.digest ?? `${id}-digest`,
    },
    lane: { kind: "direct" },
    orderKey: `${message.createdAt}\u0000${id}`,
    message,
  };
}

function activate(
  account: RuntimeAccount,
  ownerId = "owner",
  ownerInstanceId = "instance",
): OutboundOwnerContext {
  return claimOwnerTransition(
    account,
    NOW,
    { ownerId, ownerInstanceId },
    "rotated-owner",
  ).value.fence;
}

function claimRequest(
  owner: OutboundOwnerContext,
  claimId: string,
  phase: OutboundClaimRequest["phase"] = "sending",
  connectionGeneration = 1,
): OutboundClaimRequest {
  return {
    ...owner,
    connectionGeneration,
    claimId,
    phase,
  };
}

function seedNativeClaim(
  account: RuntimeAccount,
  outbound: PreparedOutboundMessage,
  request: OutboundClaimRequest,
  authorityNow = NOW,
) {
  const persisted = persistReadyTransition(account, outbound);
  if (persisted.value.kind === "conflict") {
    throw new Error("native claim fixture conflicted");
  }
  const row = account.outbound[outbound.identity.messageId];
  if (!row) throw new Error("native claim fixture is missing");
  const claim = claimForRow(request, row.identity, authorityNow);
  row.state = { kind: "claimed", claim };
  return {
    entry: entryFromRow(row),
    claim: { ...claim },
  };
}

describe("durable outbound transitions", () => {
  test("normalizes room ordering lanes to the canonical bare JID", () => {
    const message: PersistedQueuedRoomMessage = {
      kind: "room",
      id: "room-lane",
      createdAt: "2026-07-17T00:00:00.000Z",
      roomJid: " Room@Conference.Example/Nick ",
      body: "canonical lane",
    };

    expect(outboundLane(message)).toEqual({
      kind: "room",
      roomJid: "room@conference.example",
    });
  });

  test("preserves insertion order when writes share a clock tick", () => {
    const account = emptyAccount(ACCOUNT);
    const createdAt = "2026-07-17T00:00:00.000Z";

    persistReadyTransition(account, prepared("z-first", { createdAt }));
    persistReadyTransition(account, prepared("a-second", { createdAt }));

    expect(listOutboundTransition(account).value.map(({ id }) => id)).toEqual([
      "z-first",
      "a-second",
    ]);
    expect(account.outbound["a-second"]?.message.createdAt).toBe(
      "2026-07-17T00:00:00.001Z",
    );
  });

  test("preserves revision finalization, clone isolation, existing rows, and conflicts", () => {
    const account = emptyAccount(ACCOUNT);
    account.revision = 7;

    const revision = revisionTransition(account);
    expect(revision).toMatchObject({ changed: false, value: 7 });
    expect(revision.finalize?.(8)).toBe(8);

    const inserted = persistReadyTransition(account, prepared("message-1"));
    expect(inserted.value.kind).toBe("inserted");
    expect(inserted.changed).toBe(true);

    const listed = listOutboundTransition(account);
    const listedMessage = listed.value[0] as PersistedQueuedDmMessage;
    listedMessage.body = "mutated clone";
    expect(account.outbound["message-1"]?.message.body).toBe("message-1");

    const existing = persistReadyTransition(
      account,
      prepared("message-1", {
        incarnation: "retry-incarnation",
        createdAt: "2026-07-18T00:00:00.000Z",
      }),
    );
    expect(existing.changed).toBe(false);
    expect(existing.value.kind).toBe("existing");
    expect(account.outbound["message-1"]?.identity.incarnation).toBe(
      "message-1-incarnation",
    );

    const conflict = persistReadyTransition(
      account,
      prepared("message-1", { digest: "different-digest" }),
    );
    expect(conflict.changed).toBe(false);
    expect(conflict.value.kind).toBe("conflict");
  });

  test("claims only the ordered lane head and enforces owner and lease fences", () => {
    const account = emptyAccount(ACCOUNT);
    const owner = activate(account);
    persistReadyTransition(
      account,
      prepared("second", { createdAt: "2026-07-17T00:00:02.000Z" }),
    );
    persistReadyTransition(
      account,
      prepared("first", { createdAt: "2026-07-17T00:00:01.000Z" }),
    );

    const head = claimHeadTransition(
      account,
      NOW,
      { kind: "direct" },
      claimRequest(owner, "head-claim"),
    );
    expect(head.value.kind).toBe("claimed");
    if (head.value.kind !== "claimed") throw new Error("expected head claim");
    expect(head.value.entry.identity.messageId).toBe("first");

    const busy = claimHeadTransition(
      account,
      NOW + 1,
      { kind: "direct" },
      claimRequest(owner, "other-claim"),
    );
    expect(busy.value.kind).toBe("busy");

    const renewed = renewClaimTransition(
      account,
      NOW + 2,
      head.value.entry.identity,
      head.value.claim,
    );
    expect(renewed.value.kind).toBe("renewed");

    const wrongOwner = {
      ...head.value.claim,
      ownerGeneration: head.value.claim.ownerGeneration + 1,
    };
    expect(releaseClaimTransition(
      account,
      NOW + 3,
      head.value.entry.identity,
      wrongOwner,
    ).value.kind).toBe("fenced");

    const persistedOwner = account.owners[owner.ownerId]!;
    persistedOwner.leaseUntil = 100_000;
    const claim = account.outbound.first!.state;
    if (claim.kind !== "claimed") throw new Error("expected claimed row");
    claim.claim.leaseUntil = NOW + 4;
    expect(renewClaimTransition(
      account,
      NOW + 4,
      account.outbound.first!.identity,
      claim.claim,
    ).value.kind).toBe("missing");
  });

  test("live admission persists behind every predecessor state without bypassing the lane", () => {
    const account = emptyAccount(ACCOUNT);
    const owner = activate(account);
    persistReadyTransition(
      account,
      prepared("first", { createdAt: "2026-07-17T00:00:01.000Z" }),
    );

    const secondPrepared = prepared("second", {
      createdAt: "2026-07-17T00:00:02.000Z",
    });
    const queued = persistAndClaimLaneHeadTransition(
      account,
      NOW,
      secondPrepared,
      claimRequest(owner, "second-claim"),
    );
    expect(queued.value).toMatchObject({
      kind: "queued",
      entry: { identity: { messageId: "second" }, state: { kind: "ready" } },
      blocker: {
        identity: { messageId: "first" },
        state: "ready",
      },
    });
    expect(account.outbound.second?.state.kind).toBe("ready");

    const duplicate = persistAndClaimLaneHeadTransition(
      account,
      NOW,
      secondPrepared,
      claimRequest(owner, "duplicate-second-claim"),
    );
    expect(duplicate.changed).toBe(false);
    expect(duplicate.value).toMatchObject({
      kind: "queued",
      entry: { identity: { messageId: "second" } },
      blocker: { identity: { messageId: "first" }, state: "ready" },
    });
    expect(persistAndClaimLaneHeadTransition(
      account,
      NOW,
      prepared("second", { digest: "changed-digest" }),
      claimRequest(owner, "conflicting-second-claim"),
    ).value.kind).toBe("conflict");

    const firstClaim = claimHeadTransition(
      account,
      NOW,
      { kind: "direct" },
      claimRequest(owner, "first-claim"),
    );
    if (firstClaim.value.kind !== "claimed") {
      throw new Error("expected first lane claim");
    }
    const third = persistAndClaimLaneHeadTransition(
      account,
      NOW + 1,
      prepared("third", { createdAt: "2026-07-17T00:00:03.000Z" }),
      claimRequest(owner, "third-claim"),
    );
    expect(third.value).toMatchObject({
      kind: "queued",
      blocker: {
        identity: { messageId: "first" },
        state: "claimed",
        leaseUntil: firstClaim.value.claim.leaseUntil,
      },
    });

    const firstState = account.outbound.first?.state;
    if (firstState?.kind !== "claimed") {
      throw new Error("expected claimed predecessor");
    }
    firstState.claim.leaseUntil = NOW + 2;
    const fourth = persistAndClaimLaneHeadTransition(
      account,
      NOW + 2,
      prepared("fourth", { createdAt: "2026-07-17T00:00:04.000Z" }),
      claimRequest(owner, "fourth-claim"),
    );
    expect(fourth.value).toMatchObject({
      kind: "queued",
      blocker: {
        identity: { messageId: "first" },
        state: "claimed",
        leaseUntil: NOW + 2,
      },
    });

    const terminal = recordTerminalTransition(
      account,
      NOW + 3,
      firstClaim.value.entry.identity,
      "ack",
      firstState.claim,
      "first-terminal",
    );
    if (terminal.value.kind !== "recorded") {
      throw new Error("expected first terminal intent");
    }
    const fifth = persistAndClaimLaneHeadTransition(
      account,
      NOW + 3,
      prepared("fifth", { createdAt: "2026-07-17T00:00:05.000Z" }),
      claimRequest(owner, "fifth-claim"),
    );
    expect(fifth.value).toMatchObject({
      kind: "queued",
      blocker: {
        identity: { messageId: "first" },
        state: "terminal",
      },
    });

    expect(applyTerminalTransition(
      account,
      NOW + 4,
      owner,
      terminal.value.intent,
    ).value.kind).toBe("acked");
    const next = claimHeadTransition(
      account,
      NOW + 4,
      { kind: "direct" },
      claimRequest(owner, "next-claim"),
    );
    expect(next.value).toMatchObject({
      kind: "claimed",
      entry: { identity: { messageId: "second" } },
    });
  });

  test("live admission reclaims only an expired target head and isolates room lanes", () => {
    const account = emptyAccount(ACCOUNT);
    const owner = activate(account);
    const first = persistAndClaimLaneHeadTransition(
      account,
      NOW,
      prepared("direct-head"),
      claimRequest(owner, "direct-head-claim"),
    );
    if (first.value.kind !== "claimed") {
      throw new Error("expected direct head claim");
    }
    expect(persistAndClaimLaneHeadTransition(
      account,
      NOW + 1,
      prepared("direct-head"),
      claimRequest(owner, "active-retry"),
    ).value.kind).toBe("busy");

    const directState = account.outbound["direct-head"]?.state;
    if (directState?.kind !== "claimed") {
      throw new Error("expected direct head ownership");
    }
    directState.claim.leaseUntil = NOW + 2;
    const reclaimed = persistAndClaimLaneHeadTransition(
      account,
      NOW + 2,
      prepared("direct-head"),
      claimRequest(owner, "expired-retry"),
    );
    expect(reclaimed.value).toMatchObject({
      kind: "claimed",
      entry: { identity: { messageId: "direct-head" } },
      claim: { claimId: "expired-retry" },
    });

    const room = (
      id: string,
      roomJid: string,
      createdAt: string,
    ): PreparedOutboundMessage => {
      const value = prepared(id, { createdAt });
      const message = {
        kind: "room" as const,
        id,
        createdAt,
        roomJid,
        body: id,
      };
      return {
        ...value,
        lane: { kind: "room", roomJid },
        orderKey: `${createdAt}\u0000${id}`,
        message,
      };
    };
    const general = persistAndClaimLaneHeadTransition(
      account,
      NOW + 2,
      room("general-first", "general@muc.example.com", "2026-07-17T00:00:01.000Z"),
      claimRequest(owner, "general-first-claim"),
    );
    expect(general.value.kind).toBe("claimed");
    expect(persistAndClaimLaneHeadTransition(
      account,
      NOW + 2,
      room("general-second", "general@muc.example.com", "2026-07-17T00:00:02.000Z"),
      claimRequest(owner, "general-second-claim"),
    ).value).toMatchObject({
      kind: "queued",
      blocker: { identity: { messageId: "general-first" } },
    });
    expect(persistAndClaimLaneHeadTransition(
      account,
      NOW + 2,
      room("random-first", "random@muc.example.com", "2026-07-17T00:00:03.000Z"),
      claimRequest(owner, "random-first-claim"),
    ).value.kind).toBe("claimed");
  });

  test("reconciles native snapshots atomically and never partially adopts", () => {
    const account = emptyAccount(ACCOUNT);
    const owner = activate(account);
    persistReadyTransition(account, prepared("ready"));
    const retained = seedNativeClaim(
      account,
      prepared("retained"),
      claimRequest(owner, "retained-claim"),
    );
    expect(retained.entry.state.kind).toBe("claimed");

    const unresolved = reconcileResumeClaimsTransition(account, NOW + 1, {
      owner,
      connectionGeneration: 2,
      authoritativeMessageIds: ["ready", "missing"],
      phase: "resume-replay",
      claimIds: new Map([
        ["ready", "ready-resume-claim"],
        ["missing", "missing-resume-claim"],
      ]),
    });
    expect(unresolved.changed).toBe(false);
    expect(unresolved.value).toMatchObject({
      kind: "reconciled",
      claims: [],
      releasedIds: [],
      missingIds: ["missing"],
    });
    expect(account.outbound.ready?.state.kind).toBe("ready");
    expect(account.outbound.retained?.state.kind).toBe("claimed");

    const reconciled = reconcileResumeClaimsTransition(account, NOW + 1, {
      owner,
      connectionGeneration: 2,
      authoritativeMessageIds: ["ready"],
      phase: "resume-replay",
      claimIds: new Map([["ready", "ready-resume-claim"]]),
    });
    expect(reconciled.value).toMatchObject({
      kind: "reconciled",
      releasedIds: ["retained"],
      blockedIds: [],
      terminalIds: [],
      missingIds: [],
    });
    expect(account.outbound.ready?.state.kind).toBe("claimed");
    expect(account.outbound.retained?.state.kind).toBe("ready");

    const foreign = activate(account, "foreign-owner", "foreign-instance");
    seedNativeClaim(
      account,
      prepared("foreign"),
      claimRequest(foreign, "foreign-claim"),
      NOW + 2,
    );
    const blocked = reconcileResumeClaimsTransition(account, NOW + 3, {
      owner,
      connectionGeneration: 3,
      authoritativeMessageIds: ["foreign"],
      phase: "resume-replay",
      claimIds: new Map([["foreign", "blocked-claim"]]),
    });
    expect(blocked.value).toMatchObject({
      kind: "reconciled",
      claims: [],
      blockedIds: ["foreign"],
    });
    expect(account.outbound.foreign?.state.kind).toBe("claimed");
  });

  test("reconcile preflights every prepared claim identifier before mutation", () => {
    const account = emptyAccount(ACCOUNT);
    const owner = activate(account);
    persistReadyTransition(account, prepared("earlier"));
    persistReadyTransition(account, prepared("later"));
    const before = structuredClone(account);
    const serializedBefore = JSON.stringify(before);

    let failure: unknown;
    try {
      reconcileResumeClaimsTransition(account, NOW + 1, {
        owner,
        connectionGeneration: 2,
        authoritativeMessageIds: ["earlier", "later"],
        phase: "resume-replay",
        claimIds: new Map([["earlier", "earlier-resume-claim"]]),
      });
    } catch (cause) {
      failure = cause;
    }

    expect(failure).toBeInstanceOf(DOMException);
    expect(failure).toMatchObject({ name: "AbortError" });
    expect(account).toEqual(before);
    expect(JSON.stringify(account)).toBe(serializedBefore);
  });

  test("fresh-session release preserves fallback and foreign predecessor fences", () => {
    const account = emptyAccount(ACCOUNT);
    const predecessor = activate(account);
    const predecessorClaim = persistAndClaimLaneHeadTransition(
      account,
      NOW,
      prepared("predecessor"),
      claimRequest(predecessor, "predecessor-claim"),
    );
    expect(predecessorClaim.value.kind).toBe("claimed");

    const handoff = preparePagehideHandoffTransition(
      account,
      NOW + 1,
      predecessor,
      null,
      "handoff-token",
      null,
    );
    expect(handoff.value.kind).toBe("applied");
    const owner = claimOwnerTransition(
      account,
      NOW + 2,
      {
        ownerId: predecessor.ownerId,
        ownerInstanceId: "successor-instance",
        handoffToken: "handoff-token",
      },
      "unused-rotation",
    ).value.fence;

    seedNativeClaim(
      account,
      prepared("current-fallback"),
      claimRequest(owner, "current-fallback-claim", "fresh-fallback", 7),
      NOW + 2,
    );
    seedNativeClaim(
      account,
      prepared("same-owner-other-phase"),
      claimRequest(owner, "other-phase-claim", "sending", 7),
      NOW + 2,
    );
    seedNativeClaim(
      account,
      prepared("same-owner-other-generation"),
      claimRequest(owner, "other-generation-claim", "fresh-fallback", 6),
      NOW + 2,
    );
    const foreign = activate(account, "foreign-owner", "foreign-instance");
    seedNativeClaim(
      account,
      prepared("foreign"),
      claimRequest(foreign, "foreign-claim", "sending", 7),
      NOW + 2,
    );

    const beforeFencedRelease = structuredClone(account);
    const fenced = releaseForFreshSessionTransition(
      account,
      NOW + 3,
      { ...owner, ownerGeneration: owner.ownerGeneration + 1 },
      7,
    );
    expect(fenced).toEqual({ changed: false, value: null });
    expect(account).toEqual(beforeFencedRelease);

    const released = releaseForFreshSessionTransition(
      account,
      NOW + 3,
      owner,
      7,
    );
    expect(released).toEqual({
      changed: true,
      value: [
        "same-owner-other-phase",
        "same-owner-other-generation",
      ],
    });
    expect(account.outbound["current-fallback"]?.state.kind).toBe("claimed");
    expect(account.outbound["same-owner-other-generation"]?.state.kind).toBe(
      "ready",
    );
    expect(account.outbound["same-owner-other-phase"]?.state.kind).toBe(
      "ready",
    );
    expect(account.outbound.foreign?.state.kind).toBe("claimed");
    expect(account.outbound.predecessor?.state.kind).toBe("claimed");
  });

  test("records terminal intent once and applies ack, fallback, and release paths", () => {
    const account = emptyAccount(ACCOUNT);
    const owner = activate(account);

    const acked = persistAndClaimLaneHeadTransition(
      account,
      NOW,
      prepared("acked"),
      claimRequest(owner, "ack-claim"),
    );
    if (acked.value.kind !== "claimed") throw new Error("expected ack claim");
    const ackIntent = recordTerminalTransition(
      account,
      NOW + 1,
      acked.value.entry.identity,
      "ack",
      acked.value.claim,
      "ack-intent",
    );
    expect(ackIntent.value.kind).toBe("recorded");
    if (ackIntent.value.kind !== "recorded") throw new Error("expected intent");
    const duplicate = recordTerminalTransition(
      account,
      NOW + 2,
      acked.value.entry.identity,
      "ack",
      acked.value.claim,
      "ignored-intent",
    );
    expect(duplicate.changed).toBe(false);
    expect(duplicate.value).toEqual(ackIntent.value);
    expect(applyTerminalTransition(
      account,
      NOW + 2,
      owner,
      ackIntent.value.intent,
    ).value.kind).toBe("acked");
    expect(account.outbound.acked).toBeUndefined();

    const replayed = persistAndClaimLaneHeadTransition(
      account,
      NOW + 3,
      prepared("replayed"),
      claimRequest(owner, "replay-claim", "resume-replay"),
    );
    if (replayed.value.kind !== "claimed") throw new Error("expected replay claim");
    const replayIntent = recordTerminalTransition(
      account,
      NOW + 4,
      replayed.value.entry.identity,
      "native-failure",
      replayed.value.claim,
      "replay-intent",
    );
    if (replayIntent.value.kind !== "recorded") throw new Error("expected intent");
    const fallback = applyTerminalTransition(
      account,
      NOW + 5,
      owner,
      replayIntent.value.intent,
    );
    expect(fallback.value.kind).toBe("fallback");
    expect(account.outbound.replayed?.state).toMatchObject({
      kind: "claimed",
      claim: { phase: "fresh-fallback" },
    });

    const sending = seedNativeClaim(
      account,
      prepared("sending"),
      claimRequest(owner, "sending-claim"),
      NOW + 6,
    );
    const sendIntent = recordTerminalTransition(
      account,
      NOW + 7,
      sending.entry.identity,
      "native-failure",
      sending.claim,
      "send-intent",
    );
    if (sendIntent.value.kind !== "recorded") throw new Error("expected intent");
    expect(applyTerminalTransition(
      account,
      NOW + 8,
      owner,
      sendIntent.value.intent,
    ).value.kind).toBe("released");
    expect(account.outbound.sending?.state.kind).toBe("ready");
  });

  test("prunes only eligible rows and finalizes the committed revision", () => {
    const account = emptyAccount(ACCOUNT);
    account.revision = 11;
    persistReadyTransition(
      account,
      prepared("stale", { createdAt: "2026-01-01T00:00:00.000Z" }),
    );
    persistReadyTransition(
      account,
      prepared("fresh", { createdAt: "2026-07-17T00:00:00.000Z" }),
    );

    const scan = scanAndPruneTransition(
      account,
      Date.parse("2026-07-18T00:00:00.000Z"),
      Date.parse("2026-07-01T00:00:00.000Z"),
    );
    expect(scan.changed).toBe(true);
    expect(scan.value.pruned.map(({ messageId }) => messageId)).toEqual(["stale"]);
    expect(scan.value.entries.map(({ identity }) => identity.messageId)).toEqual([
      "fresh",
    ]);
    expect(scan.value.revision).toBe(11);
    expect(scan.finalize?.(12).revision).toBe(12);
  });

});
