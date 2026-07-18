import type { DurableAuthorityClock } from "../src/lib/xmpp-runtime/durable-contract";
import {
  DurableStoreEngine,
  type DurableAccountCommit,
  type DurableAccountRepository,
  type DurableAccountTransaction,
} from "../src/lib/xmpp-runtime/durable-engine";
import type { RuntimeAccount } from "../src/lib/xmpp-runtime/durable-model";

export type RecordedDurableCommit = {
  accountKey: string;
  account: RuntimeAccount;
  write: boolean;
  value: unknown;
};

export class RecordingDurableAccountRepository
implements DurableAccountRepository {
  private readonly accounts = new Map<string, unknown>();
  private nextFailure: { error: unknown } | null = null;

  readonly commits: RecordedDurableCommit[] = [];
  transactionCalls = 0;
  runCalls = 0;
  closeCalls = 0;
  beforeRun?: (
    accountKey: string,
    persisted: unknown | undefined,
  ) => void;

  async transact<T>(
    accountKey: string,
    run: DurableAccountTransaction<T>,
  ): Promise<T> {
    this.transactionCalls += 1;
    const persisted = this.accounts.has(accountKey)
      ? structuredClone(this.accounts.get(accountKey))
      : undefined;
    this.beforeRun?.(accountKey, persisted);
    if (this.nextFailure) {
      const { error } = this.nextFailure;
      this.nextFailure = null;
      throw error;
    }
    this.runCalls += 1;
    const commit = run(persisted);
    this.assertAccountKey(accountKey, commit);
    const recorded: RecordedDurableCommit = {
      accountKey,
      account: structuredClone(commit.account),
      write: commit.write,
      value: structuredClone(commit.value),
    };
    this.commits.push(recorded);
    if (commit.write) {
      this.accounts.set(accountKey, structuredClone(commit.account));
    }
    return structuredClone(commit.value);
  }

  close(): Promise<void> {
    this.closeCalls += 1;
    return Promise.resolve();
  }

  rejectNext(error: unknown): void {
    this.nextFailure = { error };
  }

  seed(accountKey: string, persisted: unknown): void {
    this.accounts.set(accountKey, structuredClone(persisted));
  }

  inspect(accountKey: string): RuntimeAccount {
    const persisted = this.accounts.get(accountKey);
    if (persisted === undefined) {
      throw new Error(`No recorded durable account for ${accountKey}`);
    }
    return structuredClone(persisted) as RuntimeAccount;
  }

  mutate(
    accountKey: string,
    change: (account: RuntimeAccount) => void,
  ): void {
    const account = this.inspect(accountKey);
    change(account);
    this.accounts.set(accountKey, structuredClone(account));
  }

  has(accountKey: string): boolean {
    return this.accounts.has(accountKey);
  }

  private assertAccountKey<T>(
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
}

export function recordingDurableStore(
  authorityClock: DurableAuthorityClock,
): {
  repository: RecordingDurableAccountRepository;
  store: DurableStoreEngine;
} {
  const repository = new RecordingDurableAccountRepository();
  return {
    repository,
    store: new DurableStoreEngine(repository, authorityClock),
  };
}
