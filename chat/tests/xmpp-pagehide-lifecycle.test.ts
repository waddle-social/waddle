import { describe, expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";
import { Effect, Exit, Scope } from "effect";
import {
  acquireXmppPagehideLifecycle,
  installXmppPagehideLifecycle,
} from "../src/lib/xmpp/pagehide-lifecycle";

class PagehideHarness {
  private readonly listeners = new Map<string, Set<EventListener>>();
  private failOnAdd: string | null = null;

  addEventListener(type: string, listener: EventListener): void {
    if (this.failOnAdd === type) {
      this.failOnAdd = null;
      throw new Error(`failed to add ${type}`);
    }
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

  failNextAdd(type: "pagehide" | "pageshow"): void {
    this.failOnAdd = type;
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

  test("a prepare failure is reported once and cannot skip refresh media suspension", () => {
    const target = new PagehideHarness();
    const suspendCall = mock(() => undefined);
    const reportFailure = mock(() => undefined);
    installXmppPagehideLifecycle(
      target as unknown as Window,
      () => ({
        prepareForPageHide: () => {
          throw new Error("snapshot failed");
        },
        resumeAfterPageShow: () => undefined,
      }),
      suspendCall,
      reportFailure,
    );

    target.dispatch("pagehide", false);

    expect(suspendCall).toHaveBeenCalledTimes(1);
    expect(reportFailure).toHaveBeenCalledTimes(1);
    expect(reportFailure.mock.calls[0]?.[0]).toMatchObject({
      operation: "prepare-xmpp",
    });
  });

  test("listener acquisition rolls pagehide back if pageshow installation fails", () => {
    const target = new PagehideHarness();
    target.failNextAdd("pageshow");

    expect(() =>
      installXmppPagehideLifecycle(
        target as unknown as Window,
        () => null,
        () => undefined,
      ),
    ).toThrow();
    expect(target.listenerCount("pagehide")).toBe(0);
    expect(target.listenerCount("pageshow")).toBe(0);
  });

  test("closing the Effect scope directly releases both listeners", () => {
    const target = new PagehideHarness();
    const scope = Effect.runSync(Scope.make());
    Effect.runSync(
      Scope.extend(
        acquireXmppPagehideLifecycle(
          target as unknown as Window,
          () => null,
          () => undefined,
        ),
        scope,
      ),
    );
    expect(target.listenerCount("pagehide")).toBe(1);
    expect(target.listenerCount("pageshow")).toBe(1);

    Effect.runSync(Scope.close(scope, Exit.succeed(undefined)));

    expect(target.listenerCount("pagehide")).toBe(0);
    expect(target.listenerCount("pageshow")).toBe(0);
  });

  test("XmppProvider owns the sole lifecycle listener", () => {
    const provider = readFileSync(new URL("../src/components/XmppProvider.vue", import.meta.url), "utf8");
    const overlay = readFileSync(new URL("../src/components/calls/CallOverlay.vue", import.meta.url), "utf8");

    expect(provider).toContain("installXmppPagehideLifecycle(");
    expect(provider).toContain("disconnectPagehideLifecycle?.();");
    expect(provider).toContain("suspendCallForPageHide,");
    expect(overlay).not.toContain("addEventListener(\"pagehide\"");
    expect(overlay).not.toContain("installXmppPagehideLifecycle");
    const unmountBlock = overlay.slice(overlay.indexOf("onBeforeUnmount(() => {"));
    expect(unmountBlock).not.toContain("tearDownActiveCall(");
  });
});
