import { describe, expect, test } from "bun:test";
import type { PersistedQueuedDmMessage } from "../src/lib/outbound-queue-store";
import type { PersistedSmResumeState } from "../src/lib/xmpp/sm-resume-types";
import {
  DurablePredecessorCapacityError,
  committedOrThrow,
} from "../src/lib/xmpp-runtime/durable-contract";
import { DurableStoreEngine } from "../src/lib/xmpp-runtime/durable-engine";
import {
  RecordingDurableAccountRepository,
  recordingDurableStore,
} from "./durable-account-repository-test-support";

const ACCOUNT = "engine@example.com";
const NOW = 10_000;

function directMessage(
  id: string,
  body = "hello",
  createdAt = "2026-07-18T00:00:00.000Z",
): PersistedQueuedDmMessage {
  return {
    kind: "dm",
    id,
    createdAt,
    peerJid: "recipient@example.com",
    body,
  };
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

describe("durable store engine", () => {
  test("store wrappers finish UUID, digest, clone, and decode preparation before transact", async () => {
    const repository = new RecordingDurableAccountRepository();
    const store = new DurableStoreEngine(repository, { now: () => NOW });
    const source = directMessage("prepared");
    let enteredRepository = false;
    repository.beforeRun = () => {
      enteredRepository = true;
      expect(source.body).toBe("changed-after-call");
    };

    const pending = store.persistReady(ACCOUNT, source);
    source.body = "changed-after-call";
    const persisted = await pending;

    expect(persisted.kind).toBe("committed");
    expect(enteredRepository).toBe(true);
    expect(repository.transactionCalls).toBe(1);
    expect(repository.runCalls).toBe(1);
    const account = repository.inspect(ACCOUNT);
    expect(account.outbound.prepared?.identity.incarnation).toMatch(
      /^[0-9a-f-]{36}$/,
    );
    expect(account.outbound.prepared?.identity.payloadDigest).toMatch(
      /^[0-9a-f]{64}$/,
    );
    expect(account.outbound.prepared?.message.body).toBe("hello");
    expect(account.outbound.prepared?.message).not.toBe(source);

    const invalidState = {
      previd: "resume",
      inboundH: 0,
      outboundH: -1,
      unhandledOutboundEntries: [],
    } as unknown as PersistedSmResumeState;
    const failedSave = await store.saveSm(
      {
        accountKey: ACCOUNT,
        ownerId: "owner",
        ownerInstanceId: "instance",
        ownerGeneration: 1,
        authorityEpoch: 0,
      },
      null,
      invalidState,
      NOW,
    );
    expect(failedSave.kind).toBe("failed");
    expect(repository.transactionCalls).toBe(1);
  });

  test("commits one adjusted timestamp and matching canonical order key", async () => {
    const { store, repository } = recordingDurableStore({ now: () => NOW });
    const predecessor = directMessage(
      "future-predecessor",
      "future",
      "2030-01-01T00:00:00.000Z",
    );
    const rollback = directMessage(
      "rollback-successor",
      "rollback",
      "2026-01-01T00:00:00.000Z",
    );

    expect(committedOrThrow(
      "engine-future-predecessor",
      await store.persistReady(ACCOUNT, predecessor),
    ).kind).toBe("inserted");
    const inserted = committedOrThrow(
      "engine-rollback-successor",
      await store.persistReady(ACCOUNT, rollback),
    );
    if (inserted.kind !== "inserted") {
      throw new Error("expected rollback successor insertion");
    }
    expect(inserted).toMatchObject({
      kind: "inserted",
      entry: {
        message: {
          id: "rollback-successor",
          createdAt: "2030-01-01T00:00:00.001Z",
        },
      },
    });

    const persisted = repository.inspect(ACCOUNT);
    const row = persisted.outbound["rollback-successor"]!;
    expect(row.message).toEqual(inserted.entry.message);
    expect(row.orderKey).toBe(
      "2030-01-01T00:00:00.001Z\u0000rollback-successor",
    );

    const retry = committedOrThrow(
      "engine-rollback-retry",
      await store.persistReady(ACCOUNT, rollback),
    );
    expect(retry.kind).toBe("existing");
    expect(repository.inspect(ACCOUNT).outbound["rollback-successor"])
      .toEqual(row);

    repository.mutate(ACCOUNT, (account) => {
      const invalid = account.outbound["future-predecessor"]!;
      invalid.message.createdAt = "invalid";
      invalid.orderKey = "invalid\u0000future-predecessor";
    });
    const beforeFailure = repository.inspect(ACCOUNT);
    const commitsBeforeFailure = repository.commits.length;
    const failed = await store.persistReady(
      ACCOUNT,
      directMessage("blocked-by-invalid"),
    );
    expect(failed.kind).toBe("failed");
    if (failed.kind !== "failed") {
      throw new Error("expected invalid lane state to fail");
    }
    expect(failed.cause).toBeInstanceOf(DOMException);
    expect((failed.cause as DOMException).name).toBe("DataError");
    expect(repository.commits).toHaveLength(commitsBeforeFailure);
    expect(repository.inspect(ACCOUNT)).toEqual(beforeFailure);
  });

  test("decodes before sampling and commits one clock, revision, finalize, and write decision", async () => {
    let now = NOW;
    let clockCalls = 0;
    const { store, repository } = recordingDurableStore({
      now: () => {
        clockCalls += 1;
        return now;
      },
    });

    expect(committedOrThrow(
      "initial-revision",
      await store.revision(ACCOUNT),
    )).toBe(0);
    expect(clockCalls).toBe(1);
    expect(repository.commits).toHaveLength(1);
    expect(repository.commits[0]?.write).toBe(true);
    expect(repository.commits[0]?.account.revision).toBe(0);

    expect(committedOrThrow(
      "stable-revision",
      await store.revision(ACCOUNT),
    )).toBe(0);
    expect(clockCalls).toBe(2);
    expect(repository.commits[1]?.write).toBe(false);

    now = 1_000;
    expect(committedOrThrow(
      "rollback-revision",
      await store.revision(ACCOUNT),
    )).toBe(1);
    expect(clockCalls).toBe(3);
    expect(repository.commits[2]).toMatchObject({
      write: true,
      account: { authorityEpoch: 1, revision: 1 },
      value: 1,
    });
  });

  test("malformed persisted graphs abort before clock sampling or commit", async () => {
    let clockCalls = 0;
    const { store, repository } = recordingDurableStore({
      now: () => {
        clockCalls += 1;
        return NOW;
      },
    });
    repository.seed(ACCOUNT, {
      accountKey: ACCOUNT,
      schemaVersion: 1,
      unexpectedRepairMarker: true,
    });

    const outcome = await store.revision(ACCOUNT);

    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.reason).toBe("unavailable");
      expect(outcome.cause).toBeInstanceOf(DOMException);
      expect((outcome.cause as DOMException).name).toBe("DataError");
    }
    expect(clockCalls).toBe(0);
    expect(repository.commits).toEqual([]);
  });

  test("classifies repository failures without retrying or committing", async () => {
    const repository = new RecordingDurableAccountRepository();
    const store = new DurableStoreEngine(repository, { now: () => NOW });
    const failures: Array<[unknown, string]> = [
      [new DurablePredecessorCapacityError(), "capacity"],
      [new DOMException("quota", "QuotaExceededError"), "quota"],
      [new DOMException("security", "SecurityError"), "security"],
      [new DOMException("abort", "AbortError"), "aborted"],
      [
        new DOMException("inactive", "TransactionInactiveError"),
        "aborted",
      ],
      [new Error("storage offline"), "unavailable"],
    ];

    for (const [failure, reason] of failures) {
      repository.rejectNext(failure);
      const outcome = await store.revision(ACCOUNT);
      expect(outcome).toMatchObject({ kind: "failed", reason, cause: failure });
    }

    expect(repository.transactionCalls).toBe(failures.length);
    expect(repository.runCalls).toBe(0);
    expect(repository.commits).toEqual([]);
  });

  test("preparation and transition failures are classified once with no partial commit", async () => {
    const { store, repository } = recordingDurableStore({ now: () => NOW });
    const uncloneable = {
      ...directMessage("uncloneable"),
      body: (() => undefined),
    } as unknown as PersistedQueuedDmMessage;

    const preparationFailure = await store.persistReady(ACCOUNT, uncloneable);
    expect(preparationFailure.kind).toBe("failed");
    expect(repository.transactionCalls).toBe(0);

    const owner = committedOrThrow(
      "claim-owner",
      await store.claimOwner(ACCOUNT, {
        ownerId: "engine-owner",
        ownerInstanceId: "engine-instance",
      }),
    ).fence;
    const saved = committedOrThrow(
      "save-sm",
      await store.saveSm(owner, null, smState("engine-sm"), NOW),
    );
    if (saved.kind !== "applied") throw new Error("expected saved SM state");
    const commitsBeforeFailure = repository.commits.length;
    const beforeFailure = repository.inspect(ACCOUNT);
    const marker = new Error("predicate failed");

    const transitionFailure = await store.consumeSm(
      owner,
      saved.value.version,
      () => {
        throw marker;
      },
    );

    expect(transitionFailure).toEqual({
      kind: "failed",
      reason: "unavailable",
      cause: marker,
    });
    expect(repository.commits).toHaveLength(commitsBeforeFailure);
    expect(repository.inspect(ACCOUNT)).toEqual(beforeFailure);
  });

  test("engine and repository clones isolate account, finalized value, and caller aliases", async () => {
    const { store, repository } = recordingDurableStore({ now: () => NOW });
    const source = directMessage("clone-isolation");
    const outcome = await store.persistReady(ACCOUNT, source);
    if (outcome.kind !== "committed" || outcome.value.kind !== "inserted") {
      throw new Error("expected inserted durable row");
    }

    source.body = "mutated-source";
    outcome.value.entry.message.body = "mutated-caller-value";
    const recorded = repository.commits.at(-1);
    if (!recorded) throw new Error("expected recorded commit");
    const recordedValue = recorded.value as typeof outcome.value;
    recordedValue.entry.message.body = "mutated-recorded-value";
    recorded.account.outbound["clone-isolation"]!.message.body =
      "mutated-recorded-account";

    expect(repository.inspect(ACCOUNT).outbound["clone-isolation"]?.message.body)
      .toBe("hello");
    expect(committedOrThrow(
      "clone-list",
      await store.list(ACCOUNT),
    )[0]?.body).toBe("hello");
  });

  test("every repository commit retains the exact requested account key", async () => {
    const repository = new RecordingDurableAccountRepository();
    const store = new DurableStoreEngine(repository, { now: () => NOW });
    const firstAccount = "first-engine@example.com";
    const secondAccount = "second-engine@example.com";

    committedOrThrow("first-key", await store.revision(firstAccount));
    committedOrThrow(
      "second-key",
      await store.persistReady(secondAccount, directMessage("second-key")),
    );

    expect(repository.commits.map((commit) => ({
      requested: commit.accountKey,
      committed: commit.account.accountKey,
    }))).toEqual([
      { requested: firstAccount, committed: firstAccount },
      { requested: secondAccount, committed: secondAccount },
    ]);
  });

  test("delegates idempotent close calls to the repository", async () => {
    const repository = new RecordingDurableAccountRepository();
    const store = new DurableStoreEngine(repository, { now: () => NOW });

    await store.close();
    await store.close();

    expect(repository.closeCalls).toBe(2);
  });
});
