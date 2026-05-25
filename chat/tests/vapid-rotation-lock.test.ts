// Coverage for the cross-tab VAPID-rotation lock. Pins:
//   * `navigator.locks` (Web Locks API) path: name + mode + critical-
//     section wrapping
//   * Promise-chain fallback when `navigator.locks` is undefined
//   * Errors in one task don't permanently block subsequent acquires
//     on the fallback path

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import {
  __resetVapidRotationLockForTests,
  VAPID_ROTATION_LOCK_NAME,
  withVapidRotationLock,
} from "../src/shell/vapid-rotation-lock";

function deferred<T>() {
  let resolveFn: (value: T) => void = () => {};
  let rejectFn: (reason: unknown) => void = () => {};
  const promise = new Promise<T>((resolve, reject) => {
    resolveFn = resolve;
    rejectFn = reject;
  });
  return { promise, resolve: resolveFn, reject: rejectFn };
}

function installNavigatorLocks() {
  const request = mock(
    async (
      _name: string,
      _options: { mode: "exclusive" } | { mode: "shared" },
      callback: () => Promise<unknown>,
    ) => callback(),
  );
  const navigatorStub = { locks: { request } } as unknown as Navigator;
  (globalThis as { navigator?: Navigator }).navigator = navigatorStub;
  return { request };
}

function installNavigatorWithoutLocks() {
  (globalThis as { navigator?: Navigator }).navigator = {} as Navigator;
}

function uninstallNavigator() {
  delete (globalThis as { navigator?: unknown }).navigator;
}

describe("vapid-rotation-lock", () => {
  beforeEach(() => {
    __resetVapidRotationLockForTests();
  });
  afterEach(() => {
    uninstallNavigator();
  });

  test("uses navigator.locks.request with exclusive mode + canonical name", async () => {
    const { request } = installNavigatorLocks();
    await withVapidRotationLock(async () => "ok");
    expect(request.mock.calls).toHaveLength(1);
    const [name, options] = request.mock.calls[0] ?? [];
    expect(name).toBe(VAPID_ROTATION_LOCK_NAME);
    expect((options as { mode: string }).mode).toBe("exclusive");
  });

  test("returns the callback's result through the lock", async () => {
    installNavigatorLocks();
    const result = await withVapidRotationLock(async () => 42);
    expect(result).toBe(42);
  });

  test("fallback path serializes overlapping calls when navigator.locks is absent", async () => {
    installNavigatorWithoutLocks();
    const order: string[] = [];
    const firstGate = deferred<void>();
    const first = withVapidRotationLock(async () => {
      order.push("first-enter");
      await firstGate.promise;
      order.push("first-exit");
      return "first";
    });
    const second = withVapidRotationLock(async () => {
      order.push("second-enter");
      return "second";
    });
    // `second` was queued after `first` and must not have entered yet.
    await Promise.resolve();
    expect(order).toEqual(["first-enter"]);
    firstGate.resolve();
    await Promise.all([first, second]);
    expect(order).toEqual(["first-enter", "first-exit", "second-enter"]);
  });

  test("fallback path: an error in one task doesn't permanently block subsequent acquires", async () => {
    installNavigatorWithoutLocks();
    const first = withVapidRotationLock(async () => {
      throw new Error("boom");
    });
    await expect(first).rejects.toThrow("boom");
    const result = await withVapidRotationLock(async () => "still works");
    expect(result).toBe("still works");
  });

  test("fallback path: missing navigator does not throw", async () => {
    // Some test runners (and Node SSR) lack `navigator` entirely; the
    // lock must still acquire via the fallback.
    uninstallNavigator();
    const result = await withVapidRotationLock(async () => "ok");
    expect(result).toBe("ok");
  });
});
