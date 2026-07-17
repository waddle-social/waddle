export type ProviderLifecycleState = "active" | "disposing" | "disposed";

export class ProviderLifecycleCancelledError extends DOMException {
  constructor() {
    super("XMPP provider lifecycle is no longer active", "InvalidStateError");
  }
}

export function isProviderLifecycleCancellation(
  error: unknown,
): error is ProviderLifecycleCancelledError {
  return error instanceof ProviderLifecycleCancelledError;
}

/**
 * Serializes provider client replacement and terminal disposal under one
 * monotonic epoch. A terminal disposal always reaches `disposed` in `finally`
 * while preserving the original operator rejection for its caller.
 */
export class ProviderLifecycle {
  private stateValue: ProviderLifecycleState = "active";
  private epochValue = 0;
  private tail: Promise<void> = Promise.resolve();
  private disposal: Promise<void> | null = null;

  get state(): ProviderLifecycleState {
    return this.stateValue;
  }

  captureActiveEpoch(): number {
    this.assertActive(this.epochValue);
    return this.epochValue;
  }

  assertActive(expectedEpoch: number): void {
    if (
      this.stateValue !== "active"
      || expectedEpoch !== this.epochValue
    ) {
      throw new ProviderLifecycleCancelledError();
    }
  }

  serialize(
    expectedEpoch: number,
    operation: (assertCurrent: () => void) => Promise<void>,
  ): Promise<void> {
    const queued = this.tail.then(async () => {
      const assertCurrent = () => this.assertActive(expectedEpoch);
      assertCurrent();
      await operation(assertCurrent);
      assertCurrent();
    });
    this.tail = queued.then(
      () => undefined,
      () => undefined,
    );
    return queued;
  }

  dispose(operation: () => Promise<void>): Promise<void> {
    if (this.disposal) return this.disposal;
    this.stateValue = "disposing";
    this.epochValue += 1;
    const queued = this.tail.then(operation);
    const terminal = queued.finally(() => {
      this.stateValue = "disposed";
    });
    this.disposal = terminal;
    this.tail = terminal.then(
      () => undefined,
      () => undefined,
    );
    return terminal;
  }
}
