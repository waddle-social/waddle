import {
  DurableStoreEngine,
  systemAuthorityClock,
  type DurableAccountCommit,
  type DurableAccountRepository,
  type DurableAccountTransaction,
} from "./durable-engine";
import type { DurableAuthorityClock } from "./durable-contract";

function assertAccountKey<T>(
  accountKey: string,
  commit: DurableAccountCommit<T>,
): void {
  if (commit.account.accountKey !== accountKey) {
    throw new DOMException(
      "Durable repository account key mismatch",
      "DataError",
    );
  }
}

/** Repository boundary exported for direct adapter conformance tests. */
export class MemoryDurableAccountRepository
implements DurableAccountRepository {
  private readonly accounts = new Map<string, unknown>();
  private transactionTail: Promise<void> = Promise.resolve();

  constructor(
    private readonly beforeTransaction?: () => Promise<void>,
  ) {}

  transact<T>(
    accountKey: string,
    run: DurableAccountTransaction<T>,
  ): Promise<T> {
    const operation = this.transactionTail.then(async () => {
      await this.beforeTransaction?.();
      const persisted = this.accounts.has(accountKey)
        ? structuredClone(this.accounts.get(accountKey))
        : undefined;
      const commit = run(persisted);
      assertAccountKey(accountKey, commit);
      const account = structuredClone(commit.account);
      const value = structuredClone(commit.value);
      if (commit.write) {
        this.accounts.set(accountKey, account);
      }
      return value;
    });
    this.transactionTail = operation.then(
      () => undefined,
      () => undefined,
    );
    return operation;
  }

  close(): Promise<void> {
    return Promise.resolve();
  }
}

export class MemoryDurableOutboundStore extends DurableStoreEngine {
  constructor(
    authorityClock: DurableAuthorityClock = systemAuthorityClock,
    beforeTransaction?: () => Promise<void>,
  ) {
    super(
      new MemoryDurableAccountRepository(beforeTransaction),
      authorityClock,
    );
  }
}
