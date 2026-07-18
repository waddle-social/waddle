import { describe, expect, test } from "bun:test";
import {
  committedOrThrow,
  type DurableAuthorityClock,
} from "../src/lib/xmpp-runtime/durable-contract";
import { emptyAccount } from "../src/lib/xmpp-runtime/durable-model";
import {
  MemoryDurableAccountRepository,
  MemoryDurableOutboundStore,
} from "../src/lib/xmpp-runtime/memory-durable-store";

const ACCOUNT = "memory-repository@example.com";

describe("memory durable repository", () => {
  test("runs the hook inside the serialized tail", async () => {
    let hookCalls = 0;
    let releaseFirst: (() => void) | undefined;
    const firstBlocked = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const store = new MemoryDurableOutboundStore(
      { now: () => 1_000 },
      async () => {
        hookCalls += 1;
        if (hookCalls === 1) await firstBlocked;
      },
    );

    const first = store.revision(`${ACCOUNT}/first`);
    await Promise.resolve();
    const second = store.revision(`${ACCOUNT}/second`);
    await Promise.resolve();
    expect(hookCalls).toBe(1);

    releaseFirst?.();
    expect(committedOrThrow("first", await first)).toBe(0);
    expect(committedOrThrow("second", await second)).toBe(0);
    expect(hookCalls).toBe(2);
  });

  test("hook rejection samples no clock and does not poison the tail", async () => {
    const marker = new Error("hook rejected");
    let hookCalls = 0;
    let clockCalls = 0;
    const clock: DurableAuthorityClock = {
      now: () => {
        clockCalls += 1;
        return 1_000;
      },
    };
    const store = new MemoryDurableOutboundStore(clock, async () => {
      hookCalls += 1;
      if (hookCalls === 1) throw marker;
    });

    const failed = await store.revision(ACCOUNT);
    expect(failed).toEqual({
      kind: "failed",
      reason: "unavailable",
      cause: marker,
    });
    expect(clockCalls).toBe(0);
    expect(committedOrThrow(
      "tail-recovery",
      await store.revision(ACCOUNT),
    )).toBe(0);
    expect(clockCalls).toBe(1);
    expect(hookCalls).toBe(2);
  });

  test("clones persisted accounts and return values before publishing them", async () => {
    const repository = new MemoryDurableAccountRepository();
    const account = emptyAccount(ACCOUNT);
    account.authorityEpoch = 2;
    const sourceValue = { nested: { value: "original" } };

    const returned = await repository.transact(ACCOUNT, () => ({
      account,
      write: true,
      value: sourceValue,
    }));
    account.authorityEpoch = 99;
    sourceValue.nested.value = "mutated-source";
    returned.nested.value = "mutated-return";

    let persisted: unknown;
    await repository.transact(ACCOUNT, (stored) => {
      persisted = stored;
      return {
        account: emptyAccount(ACCOUNT),
        write: false,
        value: undefined,
      };
    });

    expect(persisted).toMatchObject({
      accountKey: ACCOUNT,
      authorityEpoch: 2,
    });
  });

  test("rejects account-key mismatch without a write", async () => {
    const repository = new MemoryDurableAccountRepository();

    await expect(repository.transact(ACCOUNT, () => ({
      account: emptyAccount("other@example.com"),
      write: true,
      value: "should-not-commit",
    }))).rejects.toMatchObject({ name: "DataError" });

    let persisted: unknown = "not-observed";
    await repository.transact(ACCOUNT, (stored) => {
      persisted = stored;
      return {
        account: emptyAccount(ACCOUNT),
        write: false,
        value: undefined,
      };
    });
    expect(persisted).toBeUndefined();
  });

  test("close is idempotent and leaves the in-memory adapter usable", async () => {
    const store = new MemoryDurableOutboundStore({ now: () => 1_000 });

    await store.close();
    await store.close();

    expect(committedOrThrow(
      "after-close",
      await store.revision(ACCOUNT),
    )).toBe(0);
  });
});
