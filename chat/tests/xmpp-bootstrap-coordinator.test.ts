import { describe, expect, test } from "bun:test";
import { createXmppBootstrapCoordinator } from "../src/lib/xmpp/bootstrap-coordinator";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

describe("XMPP bootstrap ownership", () => {
  test("a hung physical teardown does not block its logical successor", async () => {
    const close = deferred<void>();
    let current: { dispose: () => Promise<void> } | null = {
      dispose: () => close.promise,
    };
    const coordinator = createXmppBootstrapCoordinator(
      () => current,
      (client) => { current = client; },
    );
    const successor = { dispose: async () => undefined };

    await coordinator.replace(coordinator.begin(), () => successor);

    expect(current).toBe(successor);
    close.resolve();
  });

  test("a rejected physical teardown cannot prevent its successor", async () => {
    let current: { dispose: () => Promise<void> } | null = {
      dispose: async () => { throw new Error("socket close failed"); },
    };
    const coordinator = createXmppBootstrapCoordinator(
      () => current,
      (client) => { current = client; },
    );
    const successor = { dispose: async () => undefined };

    await coordinator.replace(coordinator.begin(), () => successor);
    await Promise.resolve();

    expect(current).toBe(successor);
  });

  test("logout detaches a client even while physical teardown is hung", () => {
    const close = deferred<void>();
    let current: { dispose: () => Promise<void> } | null = {
      dispose: () => close.promise,
    };
    const coordinator = createXmppBootstrapCoordinator(
      () => current,
      (client) => { current = client; },
    );

    coordinator.detach();

    expect(current).toBeNull();
    close.resolve();
  });

  test("a ready-to-signed-out auth refresh detaches the previously ready client", () => {
    let disposeCalls = 0;
    let current: { dispose: () => Promise<void> } | null = {
      dispose: async () => { disposeCalls += 1; },
    };
    const coordinator = createXmppBootstrapCoordinator(
      () => current,
      (client) => { current = client; },
    );

    expect(coordinator.detachIfCurrent(coordinator.begin())).toBe(true);

    expect(current).toBeNull();
    expect(disposeCalls).toBe(1);
  });

  test("a ready-to-error auth refresh detaches the previously ready client", () => {
    let current: { dispose: () => Promise<void> } | null = { dispose: async () => undefined };
    const coordinator = createXmppBootstrapCoordinator(
      () => current,
      (client) => { current = client; },
    );

    expect(coordinator.detachIfCurrent(coordinator.begin())).toBe(true);
    expect(current).toBeNull();
  });

  test("a stale non-ready auth result cannot detach a newer ready client", () => {
    let current: { dispose: () => Promise<void> } | null = { dispose: async () => undefined };
    const coordinator = createXmppBootstrapCoordinator(
      () => current,
      (client) => { current = client; },
    );
    const stale = coordinator.begin();
    coordinator.begin();

    expect(coordinator.detachIfCurrent(stale)).toBe(false);
    expect(current).not.toBeNull();
  });

  test("a stale bootstrap cannot overwrite or leak a newer successor", async () => {
    const releases = deferred<void>();
    let current: { dispose: () => Promise<void> } | null = {
      dispose: () => releases.promise,
    };
    const coordinator = createXmppBootstrapCoordinator(
      () => current,
      (client) => { current = client; },
    );
    const first = coordinator.begin();
    const firstReplacement = coordinator.replace(first, () => ({ dispose: async () => undefined }));
    const second = coordinator.begin();
    const replacement = { dispose: async () => undefined };
    const secondReplacement = coordinator.replace(second, () => replacement);

    releases.resolve();
    await Promise.all([firstReplacement, secondReplacement]);

    expect(current).toBe(replacement);
  });

  test("a successor created by an invalidated bootstrap is terminally disposed", async () => {
    let current: { dispose: () => Promise<void> } | null = null;
    const coordinator = createXmppBootstrapCoordinator(
      () => current,
      (client) => { current = client; },
    );
    const generation = coordinator.begin();
    let disposeCalls = 0;
    await coordinator.replace(generation, () => {
      coordinator.invalidate();
      return { dispose: async () => { disposeCalls += 1; } };
    });

    await Promise.resolve();
    expect(disposeCalls).toBe(1);
    expect(current).toBeNull();
  });
});
