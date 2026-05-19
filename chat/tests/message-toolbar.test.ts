import { afterEach, describe, expect, test } from "bun:test";
import {
  $desktopToolbarOwnerId,
  $desktopToolbarSuppressed,
  $desktopToolbarSuspensionEpoch,
  clearDesktopToolbarOwner,
  installMessageToolbarLifecycleSuppression,
} from "../src/stores/message-toolbar";

class ListenerHarness {
  private listeners = new Map<string, Array<{ listener: EventListener; capture: boolean }>>();
  visibilityState: DocumentVisibilityState = "visible";

  addEventListener(type: string, listener: EventListener, options?: boolean | AddEventListenerOptions) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push({ listener, capture: options === true || (typeof options === "object" && options.capture === true) });
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: string, listener: EventListener, options?: boolean | EventListenerOptions) {
    const capture = options === true || (typeof options === "object" && options.capture === true);
    this.listeners.set(type, (this.listeners.get(type) ?? []).filter((entry) =>
      entry.listener !== listener || entry.capture !== capture,
    ));
  }

  dispatch(type: string) {
    const event = new Event(type);
    for (const { listener } of this.listeners.get(type) ?? []) {
      listener(event);
    }
  }

  listenerCount(type: string): number {
    return this.listeners.get(type)?.length ?? 0;
  }

  captureListenerCount(type: string): number {
    return (this.listeners.get(type) ?? []).filter((entry) => entry.capture).length;
  }
}

afterEach(() => {
  $desktopToolbarOwnerId.set(null);
  $desktopToolbarSuppressed.set(false);
  $desktopToolbarSuspensionEpoch.set(0);
});

describe("message toolbar store", () => {
  test("clears the desktop toolbar owner on page lifecycle cleanup", () => {
    $desktopToolbarOwnerId.set("message-1");

    clearDesktopToolbarOwner();

    expect($desktopToolbarOwnerId.get()).toBeNull();
  });

  test("suppresses hover/focus toolbars while a restored tab waits for fresh input", () => {
    const windowTarget = new ListenerHarness();
    const documentTarget = new ListenerHarness();
    const disconnect = installMessageToolbarLifecycleSuppression({
      windowTarget,
      documentTarget,
    });
    $desktopToolbarOwnerId.set("message-1");

    windowTarget.dispatch("blur");

    expect($desktopToolbarOwnerId.get()).toBeNull();
    expect($desktopToolbarSuppressed.get()).toBe(true);
    expect($desktopToolbarSuspensionEpoch.get()).toBe(1);
    expect(windowTarget.listenerCount("pointerdown")).toBe(1);
    expect(windowTarget.listenerCount("pointermove")).toBe(1);
    expect(windowTarget.listenerCount("keydown")).toBe(1);
    expect(windowTarget.captureListenerCount("keydown")).toBe(1);

    windowTarget.dispatch("pointermove");

    expect($desktopToolbarSuppressed.get()).toBe(false);
    expect(windowTarget.listenerCount("pointerdown")).toBe(0);
    expect(windowTarget.listenerCount("pointermove")).toBe(0);
    expect(windowTarget.listenerCount("keydown")).toBe(0);

    disconnect();
  });

  test("suppresses on hidden visibility changes but not visible ones", () => {
    const windowTarget = new ListenerHarness();
    const documentTarget = new ListenerHarness();
    const disconnect = installMessageToolbarLifecycleSuppression({
      windowTarget,
      documentTarget,
    });

    documentTarget.visibilityState = "visible";
    documentTarget.dispatch("visibilitychange");
    expect($desktopToolbarSuppressed.get()).toBe(false);
    expect($desktopToolbarSuspensionEpoch.get()).toBe(0);

    documentTarget.visibilityState = "hidden";
    documentTarget.dispatch("visibilitychange");
    expect($desktopToolbarSuppressed.get()).toBe(true);
    expect($desktopToolbarSuspensionEpoch.get()).toBe(1);

    disconnect();
  });

  test("removes lifecycle listeners and clears suppression on disconnect", () => {
    const windowTarget = new ListenerHarness();
    const documentTarget = new ListenerHarness();
    const disconnect = installMessageToolbarLifecycleSuppression({
      windowTarget,
      documentTarget,
    });

    windowTarget.dispatch("pagehide");
    expect($desktopToolbarSuppressed.get()).toBe(true);
    expect(windowTarget.listenerCount("blur")).toBe(1);
    expect(documentTarget.listenerCount("visibilitychange")).toBe(1);

    disconnect();

    expect($desktopToolbarSuppressed.get()).toBe(false);
    expect(windowTarget.listenerCount("blur")).toBe(0);
    expect(windowTarget.listenerCount("pagehide")).toBe(0);
    expect(documentTarget.listenerCount("visibilitychange")).toBe(0);
    expect(windowTarget.listenerCount("pointerdown")).toBe(0);
  });
});
