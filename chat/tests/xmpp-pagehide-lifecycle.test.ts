import { describe, expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";
import { installXmppPagehideLifecycle } from "../src/lib/xmpp/pagehide-lifecycle";

class PagehideHarness {
  private readonly listeners = new Set<EventListener>();

  addEventListener(type: string, listener: EventListener): void {
    if (type === "pagehide") this.listeners.add(listener);
  }

  removeEventListener(type: string, listener: EventListener): void {
    if (type === "pagehide") this.listeners.delete(listener);
  }

  dispatch(persisted: boolean): void {
    const event = new Event("pagehide") as PageTransitionEvent;
    Object.defineProperty(event, "persisted", { value: persisted });
    for (const listener of this.listeners) listener(event);
  }

  listenerCount(): number {
    return this.listeners.size;
  }
}

describe("persistent XMPP pagehide lifecycle", () => {
  test("ignores BFCache and prepares XMPP before call-only suspension", () => {
    const target = new PagehideHarness();
    const order: string[] = [];
    const prepareForPageHide = mock(() => order.push("xmpp"));
    const suspendCall = mock(() => order.push("call"));
    const remove = installXmppPagehideLifecycle(
      target as unknown as Window,
      () => ({ prepareForPageHide }),
      suspendCall,
    );
    expect(target.listenerCount()).toBe(1);

    target.dispatch(true);
    expect(order).toEqual([]);

    target.dispatch(false);
    expect(order).toEqual(["xmpp", "call"]);

    remove();
    expect(target.listenerCount()).toBe(0);
    target.dispatch(false);
    expect(order).toEqual(["xmpp", "call"]);
  });

  test("still suspends local call media when no XMPP client exists", () => {
    const target = new PagehideHarness();
    const suspendCall = mock(() => undefined);
    installXmppPagehideLifecycle(target as unknown as Window, () => null, suspendCall);

    target.dispatch(false);

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
