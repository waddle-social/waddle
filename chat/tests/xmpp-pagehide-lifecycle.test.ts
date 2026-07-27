import { afterEach, describe, expect, mock, test } from "bun:test";
import { installXmppPagehideLifecycle } from "../src/lib/xmpp/pagehide-lifecycle";
import { BrowserXmppClient } from "../src/lib/xmpp/client";
import {
  __setFaroForTesting,
  reportXmppPageLifecycleFailure,
} from "../src/lib/telemetry";

afterEach(() => __setFaroForTesting(null));

class LifecycleTarget {
  private readonly listeners = new Map<string, Set<EventListener>>();

  constructor(private readonly failOn?: string) {}

  addEventListener(type: string, listener: EventListener): void {
    if (type === this.failOn) throw new Error(`cannot install ${type}`);
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

  listenerCount(type: string): number {
    return this.listeners.get(type)?.size ?? 0;
  }
}

describe("XMPP page lifecycle", () => {
  test("pagehide requests the runtime acknowledgement before its synchronous snapshot", () => {
    const client = new BrowserXmppClient({
      username: "alice",
      jid: "alice@example.com/desktop",
      session_id: "token",
      xmpp_websocket_url: "wss://example.com/ws",
    });
    const order: string[] = [];
    (client as unknown as { xmpp: unknown }).xmpp = {
      request_stream_management_ack: () => {
        order.push("request-ack");
        return Promise.resolve();
      },
    };
    (client as unknown as { persistResumeStateForPageHide: () => void }).persistResumeStateForPageHide = () => {
      order.push("persist");
    };

    client.prepareForPageHide();

    expect(order).toEqual(["request-ack", "persist"]);
  });

  test("pagehide persists when the best-effort acknowledgement command throws", () => {
    const client = new BrowserXmppClient({
      username: "alice",
      jid: "alice@example.com/desktop",
      session_id: "token",
      xmpp_websocket_url: "wss://example.com/ws",
    });
    const persist = mock(() => undefined);
    (client as unknown as { xmpp: unknown }).xmpp = {
      request_stream_management_ack: () => { throw new Error("closed"); },
    };
    (client as unknown as { persistResumeStateForPageHide: () => void }).persistResumeStateForPageHide = persist;

    client.prepareForPageHide();

    expect(persist).toHaveBeenCalledTimes(1);
  });

  test("stale WASM stream-management callbacks cannot report into the current generation", () => {
    const client = new BrowserXmppClient({
      username: "alice",
      jid: "alice@example.com/desktop",
      session_id: "token",
      xmpp_websocket_url: "wss://example.com/ws",
    });
    let oldCallback: ((event: unknown) => void) | undefined;
    let currentCallback: ((event: unknown) => void) | undefined;
    const oldClient = { set_on_stream_management: (callback: (event: unknown) => void) => { oldCallback = callback; } };
    const currentClient = { set_on_stream_management: (callback: (event: unknown) => void) => { currentCallback = callback; } };
    const observed: unknown[] = [];
    client.onStreamManagement((event) => observed.push(event));

    (client as unknown as { xmpp: unknown; wireEvents: (xmpp: unknown) => void }).xmpp = oldClient;
    (client as unknown as { wireEvents: (xmpp: unknown) => void }).wireEvents(oldClient);
    (client as unknown as { xmpp: unknown; wireEvents: (xmpp: unknown) => void }).xmpp = currentClient;
    (client as unknown as { wireEvents: (xmpp: unknown) => void }).wireEvents(currentClient);

    oldCallback?.({ kind: "ack-requested", reason: "pagehide" });
    currentCallback?.({ kind: "ack-validated", progress: false });

    expect(observed).toEqual([{ kind: "ack-validated", progress: false }]);
  });

  test("has one scoped XMPP owner for pagehide and BFCache restore", () => {
    const target = new LifecycleTarget();
    const prepareForPageHide = mock(() => undefined);
    const resumeAfterPageShow = mock(() => undefined);
    const suspendCall = mock(() => undefined);
    const dispose = installXmppPagehideLifecycle(
      target as unknown as Window,
      () => ({ prepareForPageHide, resumeAfterPageShow }),
      suspendCall,
    );

    expect(target.listenerCount("pagehide")).toBe(1);
    expect(target.listenerCount("pageshow")).toBe(1);
    target.dispatch("pagehide", true);
    expect(prepareForPageHide).toHaveBeenCalledTimes(1);
    expect(suspendCall).not.toHaveBeenCalled();
    target.dispatch("pageshow", true);
    expect(resumeAfterPageShow).toHaveBeenCalledTimes(1);
    target.dispatch("pagehide", false);
    expect(prepareForPageHide).toHaveBeenCalledTimes(2);
    expect(suspendCall).toHaveBeenCalledTimes(1);

    dispose();
    dispose();
    expect(target.listenerCount("pagehide")).toBe(0);
    expect(target.listenerCount("pageshow")).toBe(0);
  });

  test("reports only closed operation data when a lifecycle callback throws", () => {
    const target = new LifecycleTarget();
    const reportFailure = mock(() => {
      throw new Error("telemetry unavailable");
    });
    const suspendCall = mock(() => undefined);
    const dispose = installXmppPagehideLifecycle(
      target as unknown as Window,
      () => ({
        prepareForPageHide: () => { throw new Error("client fault"); },
        resumeAfterPageShow: () => undefined,
      }),
      suspendCall,
      reportFailure,
    );

    target.dispatch("pagehide", false);
    expect(reportFailure).toHaveBeenCalledWith({ operation: "prepare-xmpp" });
    expect(suspendCall).toHaveBeenCalledTimes(1);
    dispose();
  });

  test("forwards every lifecycle failure once through the production Faro reporter", () => {
    const target = new LifecycleTarget();
    const events: Array<{ name: string; attributes?: Record<string, string> }> = [];
    __setFaroForTesting({
      api: {
        pushEvent: (name: string, attributes?: Record<string, string>) => {
          events.push({ name, attributes });
        },
      },
    } as never);
    const reportFailure = (failure: { operation: "prepare-xmpp" | "resume-xmpp" | "suspend-call" }) => {
      reportXmppPageLifecycleFailure(failure);
      throw new Error("collector unavailable after recording");
    };
    const dispose = installXmppPagehideLifecycle(
      target as unknown as Window,
      () => ({
        prepareForPageHide: () => { throw new Error("prepare failed"); },
        resumeAfterPageShow: () => { throw new Error("resume failed"); },
      }),
      () => { throw new Error("suspend failed"); },
      reportFailure,
    );

    target.dispatch("pagehide", false);
    target.dispatch("pageshow", true);

    expect(events).toEqual([
      {
        name: "chat.xmpp.stream_management",
        attributes: { kind: "lifecycle-failed", operation: "prepare-xmpp" },
      },
      {
        name: "chat.xmpp.stream_management",
        attributes: { kind: "lifecycle-failed", operation: "suspend-call" },
      },
      {
        name: "chat.xmpp.stream_management",
        attributes: { kind: "lifecycle-failed", operation: "resume-xmpp" },
      },
    ]);
    dispose();
  });

  test("removes the first listener when the second acquisition fails", () => {
    const target = new LifecycleTarget("pageshow");
    expect(() => installXmppPagehideLifecycle(
      target as unknown as Window,
      () => null,
      () => undefined,
    )).toThrow("failed to install XMPP page lifecycle listeners");
    expect(target.listenerCount("pagehide")).toBe(0);
    expect(target.listenerCount("pageshow")).toBe(0);
  });
});
