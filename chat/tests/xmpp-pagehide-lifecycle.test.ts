import { describe, expect, mock, test } from "bun:test";
import { installXmppPagehideLifecycle } from "../src/lib/xmpp/pagehide-lifecycle";
import {
  ProviderClientCoordinator,
  createInstrumentedProviderClientCoordinator,
} from "../src/lib/xmpp/provider-client-coordinator";
import {
  ProviderLifecycle,
  ProviderLifecycleCancelledError,
  isProviderLifecycleCancellation,
} from "../src/lib/xmpp/provider-lifecycle";

class PagehideHarness {
  private readonly listeners = new Map<string, Set<EventListener>>();

  addEventListener(type: string, listener: EventListener): void {
    const listeners = this.listeners.get(type) ?? new Set<EventListener>();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: string, listener: EventListener): void {
    this.listeners.get(type)?.delete(listener);
  }

  dispatch(type: "pagehide" | "pageshow", persisted: boolean): void {
    const event = new Event(type) as PageTransitionEvent;
    Object.defineProperty(event, "persisted", { value: persisted });
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }

  listenerCount(type: "pagehide" | "pageshow"): number {
    return this.listeners.get(type)?.size ?? 0;
  }
}

describe("persistent XMPP pagehide lifecycle", () => {
  test("persists XMPP for BFCache while keeping media suspension separate", () => {
    const target = new PagehideHarness();
    const order: string[] = [];
    const prepareForPageHide = mock(() => order.push("xmpp"));
    const resumeAfterPageShow = mock(() => order.push("reclaim"));
    const suspendCall = mock(() => order.push("call"));
    const remove = installXmppPagehideLifecycle(
      target as unknown as Window,
      () => ({ prepareForPageHide, resumeAfterPageShow }),
      suspendCall,
    );
    expect(target.listenerCount("pagehide")).toBe(1);
    expect(target.listenerCount("pageshow")).toBe(1);

    target.dispatch("pagehide", true);
    expect(order).toEqual(["xmpp"]);
    target.dispatch("pageshow", true);
    expect(order).toEqual(["xmpp", "reclaim"]);

    target.dispatch("pagehide", false);
    expect(order).toEqual(["xmpp", "reclaim", "xmpp", "call"]);
    target.dispatch("pageshow", false);
    expect(order).toEqual(["xmpp", "reclaim", "xmpp", "call"]);

    remove();
    expect(target.listenerCount("pagehide")).toBe(0);
    expect(target.listenerCount("pageshow")).toBe(0);
    target.dispatch("pagehide", false);
    target.dispatch("pageshow", true);
    expect(order).toEqual(["xmpp", "reclaim", "xmpp", "call"]);
  });

  test("still suspends local call media when no XMPP client exists", () => {
    const target = new PagehideHarness();
    const suspendCall = mock(() => undefined);
    installXmppPagehideLifecycle(target as unknown as Window, () => null, suspendCall);

    target.dispatch("pagehide", false);

    expect(suspendCall).toHaveBeenCalledTimes(1);
  });
});

describe("provider replacement lifecycle", () => {
  type TestClient = {
    id: string;
    dispose(): Promise<void>;
  };

  test("happy-path replacement disposes the predecessor before configuring and installing its successor", async () => {
    const order: string[] = [];
    const predecessor: TestClient = {
      id: "predecessor",
      async dispose() {
        order.push("dispose:predecessor");
      },
    };
    let activeClient: TestClient | null = predecessor;
    const coordinator = new ProviderClientCoordinator<string, TestClient>({
      getClient: () => activeClient,
      setClient: (client) => {
        order.push(`set:${client?.id ?? "null"}`);
        activeClient = client;
      },
      createClient: (id) => {
        order.push(`create:${id}`);
        return {
          id,
          async dispose() {
            order.push(`dispose:${id}`);
          },
        };
      },
      configureClient: (client) => {
        order.push(`configure:${client.id}`);
      },
      disposeClient: (client) => client.dispose(),
    });

    await coordinator.replace("successor");

    expect(order).toEqual([
      "set:null",
      "dispose:predecessor",
      "create:successor",
      "configure:successor",
      "set:successor",
    ]);
    expect(activeClient?.id).toBe("successor");
  });

  test("a second replacement and repeated terminal disposal retire each installed client exactly once", async () => {
    const disposeCounts = new Map<string, number>();
    let activeClient: TestClient | null = null;
    const coordinator = new ProviderClientCoordinator<string, TestClient>({
      getClient: () => activeClient,
      setClient: (client) => {
        activeClient = client;
      },
      createClient: (id) => ({
        id,
        async dispose() {
          disposeCounts.set(id, (disposeCounts.get(id) ?? 0) + 1);
        },
      }),
      configureClient: () => undefined,
      disposeClient: (client) => client.dispose(),
    });

    await coordinator.replace("first");
    await coordinator.replace("second");
    const firstDisposal = coordinator.dispose();
    const secondDisposal = coordinator.dispose();
    expect(secondDisposal).toBe(firstDisposal);
    await firstDisposal;

    expect(disposeCounts).toEqual(new Map([
      ["first", 1],
      ["second", 1],
    ]));
    expect(activeClient).toBeNull();
    expect(coordinator.state).toBe("disposed");
  });

  test("the provider integration instruments and routes status before installing a candidate", async () => {
    type Status = "connecting" | "online";
    type IntegratedClient = TestClient & {
      setStatusHandler(handler: (status: Status) => void): void;
      publish(status: Status): void;
    };

    const order: string[] = [];
    const statuses: Status[] = [];
    let activeClient: IntegratedClient | null = null;
    const coordinator = createInstrumentedProviderClientCoordinator<
      string,
      Status,
      IntegratedClient
    >({
      getClient: () => activeClient,
      setClient: (client) => {
        order.push(`set:${client?.id ?? "null"}`);
        activeClient = client;
      },
      createClient: (id) => {
        order.push(`create:${id}`);
        let statusHandler: ((status: Status) => void) | null = null;
        return {
          id,
          async dispose() {
            order.push(`dispose:${id}`);
          },
          setStatusHandler(handler) {
            order.push(`status-handler:${id}`);
            statusHandler = handler;
          },
          publish(status) {
            if (!statusHandler) throw new Error("status handler is not installed");
            statusHandler(status);
          },
        };
      },
      instrumentClient: (client) => {
        order.push(`instrument:${client.id}`);
      },
      handleStatus: (status) => {
        statuses.push(status);
      },
      disposeClient: (client) => client.dispose(),
    });

    await coordinator.replace("candidate");
    activeClient?.publish("connecting");
    activeClient?.publish("online");

    expect(order).toEqual([
      "set:null",
      "create:candidate",
      "instrument:candidate",
      "status-handler:candidate",
      "set:candidate",
    ]);
    expect(statuses).toEqual(["connecting", "online"]);
  });

  test("deferred bootstrap cannot install after terminal unmount", async () => {
    let activeClient: TestClient | null = null;
    let releaseBootstrap!: () => void;
    const bootstrapGate = new Promise<void>((resolve) => {
      releaseBootstrap = resolve;
    });
    const afterLoad = mock(() => undefined);
    const createClient = mock((id: string): TestClient => ({
      id,
      dispose: async () => undefined,
    }));
    const coordinator = new ProviderClientCoordinator<string, TestClient>({
      getClient: () => activeClient,
      setClient: (client) => {
        activeClient = client;
      },
      createClient,
      configureClient: () => undefined,
      disposeClient: (client) => client.dispose(),
    });

    const bootstrap = coordinator.bootstrap(async () => {
      await bootstrapGate;
      return "candidate";
    }, afterLoad);
    const disposal = coordinator.dispose();
    releaseBootstrap();

    const [bootstrapResult, disposalResult] = await Promise.allSettled([
      bootstrap,
      disposal,
    ]);
    expect(bootstrapResult.status).toBe("rejected");
    if (bootstrapResult.status === "rejected") {
      expect(bootstrapResult.reason).toBeInstanceOf(
        ProviderLifecycleCancelledError,
      );
    }
    expect(disposalResult.status).toBe("fulfilled");
    expect(afterLoad).not.toHaveBeenCalled();
    expect(createClient).not.toHaveBeenCalled();
    expect(activeClient).toBeNull();
    expect(coordinator.state).toBe("disposed");
  });

  test("candidate setup failure disposes that candidate exactly once", async () => {
    let activeClient: TestClient | null = null;
    const setupFailure = new Error("candidate setup failed");
    let disposeCalls = 0;
    const candidate: TestClient = {
      id: "candidate",
      async dispose() {
        disposeCalls += 1;
      },
    };
    const coordinator = new ProviderClientCoordinator<string, TestClient>({
      getClient: () => activeClient,
      setClient: (client) => {
        activeClient = client;
      },
      createClient: () => candidate,
      configureClient: () => {
        throw setupFailure;
      },
      disposeClient: (client) => client.dispose(),
    });

    const result = await Promise.allSettled([
      coordinator.replace("candidate"),
    ]);
    expect(result[0]?.status).toBe("rejected");
    if (result[0]?.status === "rejected") {
      expect(result[0].reason).toBe(setupFailure);
    }
    expect(disposeCalls).toBe(1);
    expect(activeClient).toBeNull();
  });

  test("predecessor failure prevents replacement and preserves error identity", async () => {
    const disposalFailure = new Error("predecessor disposal failed");
    const predecessor: TestClient = {
      id: "predecessor",
      async dispose() {
        throw disposalFailure;
      },
    };
    let activeClient: TestClient | null = predecessor;
    const createClient = mock((id: string): TestClient => ({
      id,
      dispose: async () => undefined,
    }));
    const coordinator = new ProviderClientCoordinator<string, TestClient>({
      getClient: () => activeClient,
      setClient: (client) => {
        activeClient = client;
      },
      createClient,
      configureClient: () => undefined,
      disposeClient: (client) => client.dispose(),
    });

    const result = await Promise.allSettled([
      coordinator.replace("successor"),
    ]);
    expect(result[0]?.status).toBe("rejected");
    if (result[0]?.status === "rejected") {
      expect(result[0].reason).toBe(disposalFailure);
    }
    expect(createClient).not.toHaveBeenCalled();
    expect(activeClient).toBeNull();
  });

  test("terminal disposal preserves the original predecessor error identity", async () => {
    const disposalFailure = new Error("terminal predecessor disposal failed");
    const predecessor: TestClient = {
      id: "predecessor",
      async dispose() {
        throw disposalFailure;
      },
    };
    let activeClient: TestClient | null = predecessor;
    const coordinator = new ProviderClientCoordinator<string, TestClient>({
      getClient: () => activeClient,
      setClient: (client) => {
        activeClient = client;
      },
      createClient: () => predecessor,
      configureClient: () => undefined,
      disposeClient: (client) => client.dispose(),
    });

    const first = coordinator.dispose();
    const second = coordinator.dispose();
    expect(second).toBe(first);
    const result = await Promise.allSettled([first]);
    expect(result[0]?.status).toBe("rejected");
    if (result[0]?.status === "rejected") {
      expect(result[0].reason).toBe(disposalFailure);
    }
    expect(activeClient).toBeNull();
    expect(coordinator.state).toBe("disposed");
  });

  test("terminal disposal cancels queued replacement work with the exact token", async () => {
    const lifecycle = new ProviderLifecycle();
    const epoch = lifecycle.captureActiveEpoch();
    let releaseFirst!: () => void;
    let markFirstStarted!: () => void;
    const firstGate = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const firstStarted = new Promise<void>((resolve) => {
      markFirstStarted = resolve;
    });
    const ran: string[] = [];
    const first = lifecycle.serialize(epoch, async (assertCurrent) => {
      ran.push("first-start");
      markFirstStarted();
      await firstGate;
      assertCurrent();
      ran.push("first-finish");
    });
    await firstStarted;
    const second = lifecycle.serialize(epoch, async () => {
      ran.push("second");
    });
    const disposal = lifecycle.dispose(async () => {
      ran.push("dispose");
    });

    releaseFirst();
    const [firstResult, secondResult, disposalResult] = await Promise.allSettled([
      first,
      second,
      disposal,
    ]);

    expect(firstResult.status).toBe("rejected");
    expect(secondResult.status).toBe("rejected");
    if (firstResult.status === "rejected") {
      expect(firstResult.reason).toBeInstanceOf(ProviderLifecycleCancelledError);
      expect(isProviderLifecycleCancellation(firstResult.reason)).toBe(true);
    }
    if (secondResult.status === "rejected") {
      expect(secondResult.reason).toBeInstanceOf(ProviderLifecycleCancelledError);
    }
    expect(disposalResult.status).toBe("fulfilled");
    expect(ran).toEqual(["first-start", "dispose"]);
    expect(lifecycle.state).toBe("disposed");
  });

  test("failed terminal disposal is memoized, observable, and still reaches disposed", async () => {
    const lifecycle = new ProviderLifecycle();
    const operatorFailure = new Error("predecessor disposal failed");
    const first = lifecycle.dispose(async () => {
      throw operatorFailure;
    });
    const second = lifecycle.dispose(async () => {
      throw new Error("must not run");
    });

    expect(second).toBe(first);
    const result = await Promise.allSettled([first]);
    expect(result[0]?.status).toBe("rejected");
    if (result[0]?.status === "rejected") {
      expect(result[0].reason).toBe(operatorFailure);
    }
    expect(lifecycle.state).toBe("disposed");
  });
});
