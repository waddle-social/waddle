export const ROOM_JOIN_RETRY_BASE_DELAY_MS = 2_000;
export const ROOM_JOIN_RETRY_MAX_DELAY_MS = 60_000;

export type RoomJoinRetryTimer = {
  setTimeout(callback: () => void, delayMs: number): ReturnType<typeof setTimeout>;
  clearTimeout(handle: ReturnType<typeof setTimeout>): void;
};

type RoomJoinRetryOptions = {
  timer?: RoomJoinRetryTimer;
  random?: () => number;
};

type ScheduledRoomJoinRetry = {
  promise: Promise<void>;
  timer: ReturnType<typeof setTimeout>;
  reject: (error: Error) => void;
};

type ScheduleRoomJoinRetryOptions = {
  isEligible: () => boolean;
  retry: () => Promise<void>;
};

const defaultTimer: RoomJoinRetryTimer = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (handle) => clearTimeout(handle),
};

/**
 * Equal-jitter exponential backoff. The lower half of each exponential
 * window is skipped so a transient failure cannot immediately hammer the
 * room again, while the randomized upper half spreads clients that were
 * rejected in the same server-side burst.
 */
export function roomJoinRetryDelayMs(
  failureIndex: number,
  random: () => number = Math.random,
): number {
  const exponent = Math.max(0, Math.floor(failureIndex));
  const exponentialDelay = Math.min(
    ROOM_JOIN_RETRY_MAX_DELAY_MS,
    ROOM_JOIN_RETRY_BASE_DELAY_MS * (2 ** Math.min(exponent, 30)),
  );
  const jitter = 0.5 + (Math.min(1, Math.max(0, random())) * 0.5);
  return Math.round(exponentialDelay * jitter);
}

/**
 * Owns one pending retry per canonical room JID. Callers arriving during the
 * backoff window receive the same promise, so they cannot turn one rejected
 * attempt into a burst of immediate wire retries.
 */
export class RoomJoinRetryCoordinator {
  private readonly timer: RoomJoinRetryTimer;
  private readonly random: () => number;
  private readonly failureCounts = new Map<string, number>();
  private readonly scheduled = new Map<string, ScheduledRoomJoinRetry>();

  constructor(options: RoomJoinRetryOptions = {}) {
    this.timer = options.timer ?? defaultTimer;
    this.random = options.random ?? Math.random;
  }

  pending(roomKey: string): Promise<void> | undefined {
    return this.scheduled.get(roomKey)?.promise;
  }

  schedule(roomKey: string, options: ScheduleRoomJoinRetryOptions): Promise<void> {
    const existing = this.scheduled.get(roomKey);
    if (existing) return existing.promise;

    const failureIndex = this.failureCounts.get(roomKey) ?? 0;
    this.failureCounts.set(roomKey, failureIndex + 1);

    let resolveRetry!: () => void;
    let rejectRetry!: (error: Error) => void;
    const promise = new Promise<void>((resolve, reject) => {
      resolveRetry = resolve;
      rejectRetry = reject;
    });
    const entry = {} as ScheduledRoomJoinRetry;
    const timer = this.timer.setTimeout(() => {
      void this.run(roomKey, entry, options, resolveRetry, rejectRetry);
    }, roomJoinRetryDelayMs(failureIndex, this.random));
    Object.assign(entry, { promise, timer, reject: rejectRetry });
    this.scheduled.set(roomKey, entry);
    // The retry runs independently of any one UI listener. Keep cancellation
    // or a failed scheduled attempt from becoming an unhandled rejection when
    // nobody is currently awaiting the shared promise.
    void promise.catch(() => undefined);
    return promise;
  }

  cancel(roomKey: string): void {
    const entry = this.scheduled.get(roomKey);
    this.failureCounts.delete(roomKey);
    if (!entry) return;
    this.scheduled.delete(roomKey);
    this.timer.clearTimeout(entry.timer);
    entry.reject(new Error("Room join retry cancelled"));
  }

  cancelAll(): void {
    for (const roomKey of [...this.scheduled.keys()]) this.cancel(roomKey);
    this.failureCounts.clear();
  }

  reset(roomKey: string): void {
    this.cancel(roomKey);
    this.failureCounts.delete(roomKey);
  }

  private async run(
    roomKey: string,
    entry: ScheduledRoomJoinRetry,
    options: ScheduleRoomJoinRetryOptions,
    resolve: () => void,
    reject: (error: Error) => void,
  ): Promise<void> {
    if (this.scheduled.get(roomKey) !== entry) return;
    this.scheduled.delete(roomKey);
    if (!options.isEligible()) {
      this.failureCounts.delete(roomKey);
      reject(new Error("Room join retry cancelled"));
      return;
    }
    try {
      await options.retry();
      this.failureCounts.delete(roomKey);
      resolve();
    } catch (error) {
      // A retryable failure schedules the next window synchronously from the
      // join error handler before this promise rejects. If it did not, the
      // chain ended for another reason and a future failure starts fresh.
      if (!this.scheduled.has(roomKey)) this.failureCounts.delete(roomKey);
      reject(error instanceof Error ? error : new Error(String(error)));
    }
  }
}
