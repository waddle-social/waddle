import { describe, expect, mock, test } from "bun:test";
import {
  createVirtualTimelineElementMeasureScheduler,
  createVirtualTimelineMeasureScheduler,
  installVirtualTimelineMeasurementRecovery,
} from "../src/ui/virtual-timeline-measurement";

class ListenerHarness {
  private listeners = new Map<string, Array<{ listener: EventListener; capture: boolean }>>();

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

  dispatchFromDescendant(type: string, target: object) {
    const event = { target } as Event;
    for (const { listener, capture } of this.listeners.get(type) ?? []) {
      if (capture) listener(event);
    }
  }

  listenerCount(type: string): number {
    return this.listeners.get(type)?.length ?? 0;
  }
}

class MeasuredRow {
  closest(selector: string) {
    return selector === "[data-index]" ? this : null;
  }
}

class MediaChild {
  constructor(private readonly row: MeasuredRow) {}

  closest(selector: string) {
    return this.row.closest(selector);
  }
}

class ScrollElementHarness extends ListenerHarness {
  private readonly rows = new Set<object>();

  addMeasuredRow(row: object) {
    this.rows.add(row);
  }

  contains(node: object) {
    return this.rows.has(node);
  }
}

function createFrameHarness() {
  let nextId = 1;
  const callbacks = new Map<number, FrameRequestCallback>();
  return {
    request(callback: FrameRequestCallback) {
      const id = nextId++;
      callbacks.set(id, callback);
      return id;
    },
    cancel(id: number) {
      callbacks.delete(id);
    },
    flushFrame() {
      const pending = Array.from(callbacks.entries());
      callbacks.clear();
      for (const [, callback] of pending) {
        callback(0);
      }
    },
    pendingCount() {
      return callbacks.size;
    },
  };
}

describe("virtual timeline measurement recovery", () => {
  test("coalesces repeated measure requests until the next layout frame", () => {
    const frames = createFrameHarness();
    const measure = mock(() => {});
    const scheduler = createVirtualTimelineMeasureScheduler(measure, {
      requestAnimationFrame: frames.request,
      cancelAnimationFrame: frames.cancel,
    });

    scheduler.scheduleMeasure();
    scheduler.scheduleMeasure();

    expect(frames.pendingCount()).toBe(1);
    expect(measure).toHaveBeenCalledTimes(0);

    frames.flushFrame();
    expect(measure).toHaveBeenCalledTimes(0);

    frames.flushFrame();
    expect(measure).toHaveBeenCalledTimes(1);
  });

  test("coalesces targeted row measurements until the next layout frame", () => {
    const frames = createFrameHarness();
    const measureElement = mock(() => {});
    const row = new MeasuredRow() as unknown as HTMLElement;
    const scheduler = createVirtualTimelineElementMeasureScheduler(measureElement, {
      requestAnimationFrame: frames.request,
      cancelAnimationFrame: frames.cancel,
    });

    scheduler.scheduleMeasure(row);
    scheduler.scheduleMeasure(row);

    expect(frames.pendingCount()).toBe(1);
    expect(measureElement).toHaveBeenCalledTimes(0);

    frames.flushFrame();
    expect(measureElement).toHaveBeenCalledTimes(0);

    frames.flushFrame();
    expect(measureElement).toHaveBeenCalledTimes(1);
    expect(measureElement).toHaveBeenCalledWith(row);
  });

  test("remeasures descendant media rows by capture while full measuring on restore events", () => {
    const frames = createFrameHarness();
    const scrollElement = new ScrollElementHarness();
    const windowTarget = new ListenerHarness();
    const documentTarget = Object.assign(new ListenerHarness(), {
      visibilityState: "visible" as DocumentVisibilityState,
    });
    const measure = mock(() => {});
    const measureElement = mock(() => {});
    const row = new MeasuredRow();
    const media = new MediaChild(row);
    scrollElement.addMeasuredRow(row);
    const disconnect = installVirtualTimelineMeasurementRecovery({
      scrollElement: scrollElement as unknown as HTMLElement,
      windowTarget,
      documentTarget,
      measure,
      measureElement,
      requestAnimationFrame: frames.request,
      cancelAnimationFrame: frames.cancel,
    });

    frames.flushFrame();
    frames.flushFrame();
    expect(measure).toHaveBeenCalledTimes(1);
    expect(measureElement).toHaveBeenCalledTimes(0);

    scrollElement.dispatchFromDescendant("load", media);
    scrollElement.dispatchFromDescendant("loadedmetadata", media);
    frames.flushFrame();
    frames.flushFrame();
    expect(measure).toHaveBeenCalledTimes(1);
    expect(measureElement).toHaveBeenCalledTimes(1);
    expect(measureElement).toHaveBeenCalledWith(row);

    windowTarget.dispatch("focus");
    frames.flushFrame();
    frames.flushFrame();
    expect(measure).toHaveBeenCalledTimes(2);

    documentTarget.visibilityState = "hidden";
    documentTarget.dispatch("visibilitychange");
    frames.flushFrame();
    frames.flushFrame();
    expect(measure).toHaveBeenCalledTimes(2);

    documentTarget.visibilityState = "visible";
    documentTarget.dispatch("visibilitychange");
    windowTarget.dispatch("pageshow");
    frames.flushFrame();
    frames.flushFrame();
    expect(measure).toHaveBeenCalledTimes(3);

    disconnect();
  });

  test("removes listeners and cancels pending measurements", () => {
    const frames = createFrameHarness();
    const scrollElement = new ListenerHarness();
    const windowTarget = new ListenerHarness();
    const documentTarget = Object.assign(new ListenerHarness(), {
      visibilityState: "visible" as DocumentVisibilityState,
    });
    const measure = mock(() => {});
    const disconnect = installVirtualTimelineMeasurementRecovery({
      scrollElement: scrollElement as unknown as HTMLElement,
      windowTarget,
      documentTarget,
      measure,
      requestAnimationFrame: frames.request,
      cancelAnimationFrame: frames.cancel,
    });

    expect(scrollElement.listenerCount("load")).toBe(1);
    expect(windowTarget.listenerCount("focus")).toBe(1);
    expect(documentTarget.listenerCount("visibilitychange")).toBe(1);

    disconnect();
    frames.flushFrame();
    frames.flushFrame();

    expect(measure).toHaveBeenCalledTimes(0);
    expect(scrollElement.listenerCount("load")).toBe(0);
    expect(windowTarget.listenerCount("focus")).toBe(0);
    expect(documentTarget.listenerCount("visibilitychange")).toBe(0);
  });
});
