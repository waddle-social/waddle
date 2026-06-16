import { describe, expect, mock, test } from "bun:test";
import { createScrollFrameScheduler } from "../src/ui/scroll-frame";

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
      for (const [, callback] of pending) callback(0);
    },
    pendingCount() {
      return callbacks.size;
    },
  };
}

describe("scroll frame scheduler", () => {
  test("coalesces repeated scroll work into one frame", () => {
    const frames = createFrameHarness();
    const run = mock(() => {});
    const scheduler = createScrollFrameScheduler(run, {
      requestAnimationFrame: frames.request,
      cancelAnimationFrame: frames.cancel,
    });

    scheduler.schedule();
    scheduler.schedule();

    expect(frames.pendingCount()).toBe(1);
    expect(run).toHaveBeenCalledTimes(0);

    frames.flushFrame();

    expect(run).toHaveBeenCalledTimes(1);
  });

  test("flush runs immediately and cancels the pending frame", () => {
    const frames = createFrameHarness();
    const run = mock(() => {});
    const scheduler = createScrollFrameScheduler(run, {
      requestAnimationFrame: frames.request,
      cancelAnimationFrame: frames.cancel,
    });

    scheduler.schedule();
    scheduler.flush();

    expect(frames.pendingCount()).toBe(0);
    expect(run).toHaveBeenCalledTimes(1);

    frames.flushFrame();
    expect(run).toHaveBeenCalledTimes(1);
  });

  test("disconnect cancels pending work", () => {
    const frames = createFrameHarness();
    const run = mock(() => {});
    const scheduler = createScrollFrameScheduler(run, {
      requestAnimationFrame: frames.request,
      cancelAnimationFrame: frames.cancel,
    });

    scheduler.schedule();
    scheduler.disconnect();

    expect(frames.pendingCount()).toBe(0);

    frames.flushFrame();
    scheduler.schedule();

    expect(run).toHaveBeenCalledTimes(0);
  });
});
