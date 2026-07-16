import { describe, expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";
import { installXmppPagehideLifecycle } from "../src/lib/xmpp/pagehide-lifecycle";

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

  test("XmppProvider owns the sole lifecycle listener", () => {
    const provider = readFileSync(new URL("../src/components/XmppProvider.vue", import.meta.url), "utf8");
    const overlay = readFileSync(new URL("../src/components/calls/CallOverlay.vue", import.meta.url), "utf8");

    expect(provider).toContain("installXmppPagehideLifecycle(");
    expect(provider).toContain("disconnectPagehideLifecycle?.();");
    expect(overlay).not.toContain("addEventListener(\"pagehide\"");
    expect(overlay).not.toContain("installXmppPagehideLifecycle");
    const unmountBlock = overlay.slice(overlay.indexOf("onBeforeUnmount(() => {"));
    expect(unmountBlock).not.toContain("tearDownActiveCall(");
  });
});
