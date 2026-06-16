type AnimationFrameRequest = (callback: FrameRequestCallback) => number;
type AnimationFrameCancel = (handle: number) => void;

export function requestFrame(callback: FrameRequestCallback): number {
  if (typeof requestAnimationFrame === "function") {
    return requestAnimationFrame(callback);
  }
  return setTimeout(() => callback(Date.now()), 16) as unknown as number;
}

export function cancelFrame(handle: number): void {
  if (typeof cancelAnimationFrame === "function") {
    cancelAnimationFrame(handle);
    return;
  }
  clearTimeout(handle as unknown as ReturnType<typeof setTimeout>);
}

interface RafScheduler {
  /** Request that `run` execute on the next settled layout frame. */
  schedule: () => void;
  /** Cancel any pending frame and stop the scheduler permanently. */
  disconnect: () => void;
}

/**
 * Coalesces repeated `schedule()` calls into a single `run()` per layout
 * frame.
 *
 * The callback is dispatched on a *double* `requestAnimationFrame`: the
 * inner frame fires after the browser has produced layout for the pending
 * frame, which is the safe point to read geometry
 * (`getBoundingClientRect`, `scrollTop`, …) without forcing a synchronous
 * reflow mid-event. This makes it the right tool for high-frequency event
 * sources — scroll, resize — whose handlers would otherwise thrash layout
 * by reading geometry on every event.
 */
export function createRafScheduler(
  run: () => void,
  options: {
    requestAnimationFrame?: AnimationFrameRequest;
    cancelAnimationFrame?: AnimationFrameCancel;
  } = {},
): RafScheduler {
  const request = options.requestAnimationFrame ?? requestFrame;
  const cancel = options.cancelAnimationFrame ?? cancelFrame;
  let frame: number | null = null;
  let disposed = false;

  function schedule() {
    if (disposed || frame !== null) return;
    frame = request(() => {
      frame = request(() => {
        frame = null;
        if (!disposed) run();
      });
    });
  }

  function disconnect() {
    disposed = true;
    if (frame === null) return;
    cancel(frame);
    frame = null;
  }

  return { schedule, disconnect };
}
