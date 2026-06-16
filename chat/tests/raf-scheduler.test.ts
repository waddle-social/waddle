import { describe, expect, mock, test } from "bun:test";
import { createRafScheduler } from "../src/ui/raf-scheduler";

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

describe("createRafScheduler", () => {
  test("coalesces repeated schedule calls into one run on the settled frame", () => {
    const frames = createFrameHarness();
    const run = mock(() => {});
    const scheduler = createRafScheduler(run, {
      requestAnimationFrame: frames.request,
      cancelAnimationFrame: frames.cancel,
    });

    scheduler.schedule();
    scheduler.schedule();
    scheduler.schedule();

    // Many calls collapse to a single pending frame request.
    expect(frames.pendingCount()).toBe(1);
    expect(run).toHaveBeenCalledTimes(0);

    // First frame only chains into the second (layout-settled) frame.
    frames.flushFrame();
    expect(run).toHaveBeenCalledTimes(0);

    // Run fires on the inner frame, after layout has settled.
    frames.flushFrame();
    expect(run).toHaveBeenCalledTimes(1);
  });

  test("allows a fresh run to be scheduled after the previous one fired", () => {
    const frames = createFrameHarness();
    const run = mock(() => {});
    const scheduler = createRafScheduler(run, {
      requestAnimationFrame: frames.request,
      cancelAnimationFrame: frames.cancel,
    });

    scheduler.schedule();
    frames.flushFrame();
    frames.flushFrame();
    expect(run).toHaveBeenCalledTimes(1);

    scheduler.schedule();
    frames.flushFrame();
    frames.flushFrame();
    expect(run).toHaveBeenCalledTimes(2);
  });

  test("disconnect cancels a pending frame and stops further runs", () => {
    const frames = createFrameHarness();
    const run = mock(() => {});
    const scheduler = createRafScheduler(run, {
      requestAnimationFrame: frames.request,
      cancelAnimationFrame: frames.cancel,
    });

    scheduler.schedule();
    expect(frames.pendingCount()).toBe(1);

    scheduler.disconnect();
    expect(frames.pendingCount()).toBe(0);

    frames.flushFrame();
    frames.flushFrame();
    expect(run).toHaveBeenCalledTimes(0);

    // Scheduling after disconnect is a no-op.
    scheduler.schedule();
    expect(frames.pendingCount()).toBe(0);
    frames.flushFrame();
    frames.flushFrame();
    expect(run).toHaveBeenCalledTimes(0);
  });

  test("does not invoke run if disconnected between the outer and inner frame", () => {
    const frames = createFrameHarness();
    const run = mock(() => {});
    const scheduler = createRafScheduler(run, {
      requestAnimationFrame: frames.request,
      cancelAnimationFrame: frames.cancel,
    });

    scheduler.schedule();
    frames.flushFrame(); // outer frame ran; inner frame is now pending
    scheduler.disconnect();
    frames.flushFrame();
    expect(run).toHaveBeenCalledTimes(0);
  });
});
