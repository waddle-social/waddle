import { describe, expect, mock, test } from "bun:test";
import {
  CALL_TRANSPORT_RECOVERY_GRACE_MS,
  createCallTransportRecovery,
} from "../src/lib/calls/call-transport-recovery";
import type { TimerClock } from "../src/lib/calls/timer-clock";

type ScheduledTimer = {
  callback: () => void;
  dueAtMs: number;
  id: number;
};

class FakeClock implements TimerClock {
  private currentMs = 0;
  private nextId = 1;
  private timers: ScheduledTimer[] = [];

  now(): number {
    return this.currentMs;
  }

  setTimeout(callback: () => void, delayMs: number): ReturnType<typeof setTimeout> {
    const id = this.nextId++;
    this.timers.push({ callback, dueAtMs: this.currentMs + delayMs, id });
    return id as unknown as ReturnType<typeof setTimeout>;
  }

  clearTimeout(timer: ReturnType<typeof setTimeout>): void {
    const id = timer as unknown as number;
    this.timers = this.timers.filter((candidate) => candidate.id !== id);
  }

  advanceBy(delayMs: number): void {
    const targetMs = this.currentMs + delayMs;
    while (true) {
      const next = this.timers
        .filter((timer) => timer.dueAtMs <= targetMs)
        .sort((left, right) => left.dueAtMs - right.dueAtMs)[0];
      if (!next) break;
      this.timers = this.timers.filter((timer) => timer.id !== next.id);
      this.currentMs = next.dueAtMs;
      next.callback();
    }
    this.currentMs = targetMs;
  }
}

function setup(active = true) {
  let mediaActive = active;
  const clock = new FakeClock();
  const teardown = mock(() => undefined);
  const recovery = createCallTransportRecovery({
    isCallMediaActive: () => mediaActive,
    teardown,
    clock,
  });
  return {
    clock,
    recovery,
    setMediaActive: (value: boolean) => {
      mediaActive = value;
    },
    teardown,
  };
}

describe("call transport recovery", () => {
  test("defers teardown while call media is active", () => {
    const { clock, recovery, teardown } = setup();

    expect(recovery.onTransportLost()).toBe("deferred");
    clock.advanceBy(CALL_TRANSPORT_RECOVERY_GRACE_MS - 1);

    expect(teardown).not.toHaveBeenCalled();
  });

  test("does not defer teardown when call media is not active", () => {
    const { recovery, teardown } = setup(false);

    expect(recovery.onTransportLost()).toBe("not-deferred");

    expect(teardown).not.toHaveBeenCalled();
  });

  test("tears down once when the recovery grace expires", () => {
    const { clock, recovery, teardown } = setup();
    recovery.onTransportLost();

    clock.advanceBy(CALL_TRANSPORT_RECOVERY_GRACE_MS);
    clock.advanceBy(CALL_TRANSPORT_RECOVERY_GRACE_MS);

    expect(teardown).toHaveBeenCalledTimes(1);
  });

  test("session readiness cancels deferred teardown", () => {
    const { clock, recovery, teardown } = setup();
    recovery.onTransportLost();

    recovery.onTransportReady();
    clock.advanceBy(CALL_TRANSPORT_RECOVERY_GRACE_MS);

    expect(teardown).not.toHaveBeenCalled();
  });

  test("dispose cancels deferred teardown", () => {
    const { clock, recovery, teardown } = setup();
    recovery.onTransportLost();

    recovery.dispose();
    clock.advanceBy(CALL_TRANSPORT_RECOVERY_GRACE_MS);

    expect(teardown).not.toHaveBeenCalled();
  });

  test("expiry is a no-op after the active call ends locally", () => {
    const { clock, recovery, setMediaActive, teardown } = setup();
    recovery.onTransportLost();

    setMediaActive(false);
    clock.advanceBy(CALL_TRANSPORT_RECOVERY_GRACE_MS);

    expect(teardown).not.toHaveBeenCalled();
  });
});
