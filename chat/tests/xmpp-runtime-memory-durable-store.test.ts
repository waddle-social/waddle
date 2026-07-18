import { describe, expect, test } from "bun:test";
import {
  committedOrThrow,
  type DurableAuthorityClock,
} from "../src/lib/xmpp-runtime/durable-contract";
import type { PersistedQueuedDmMessage } from "../src/lib/outbound-queue-store";
import { MemoryDurableOutboundStore } from "../src/lib/xmpp-runtime/memory-durable-store";

const ACCOUNT = "memory-repository@example.com";

function directMessage(id: string): PersistedQueuedDmMessage {
  return {
    kind: "dm",
    id,
    createdAt: "2026-07-18T00:00:00.000Z",
    peerJid: "recipient@example.com",
    body: "original",
  };
}

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
    const store = new MemoryDurableOutboundStore({ now: () => 1_000 });
    const source = directMessage("clone-isolation");
    const persisted = committedOrThrow(
      "persist-clone",
      await store.persistReady(ACCOUNT, source),
    );
    if (persisted.kind !== "inserted") throw new Error("expected insert");

    source.body = "mutated-source";
    persisted.entry.message.body = "mutated-return";
    const firstRead = committedOrThrow("first-read", await store.list(ACCOUNT));
    firstRead[0]!.body = "mutated-first-read";

    expect(committedOrThrow(
      "second-read",
      await store.list(ACCOUNT),
    )).toEqual([directMessage("clone-isolation")]);
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
