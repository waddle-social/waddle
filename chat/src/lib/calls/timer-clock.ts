/**
 * Injectable wall-clock + timer surface shared by the call-scoped
 * schedulers (XEP-0215 credential refresh, transport-loss recovery), so
 * their tests drive time deterministically with one fake.
 */
export type TimerClock = {
  now(): number;
  setTimeout(callback: () => void, delayMs: number): ReturnType<typeof setTimeout>;
  clearTimeout(timer: ReturnType<typeof setTimeout>): void;
};

export const browserTimerClock: TimerClock = {
  now: () => Date.now(),
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer),
};
