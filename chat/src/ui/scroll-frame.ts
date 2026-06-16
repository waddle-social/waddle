type AnimationFrameRequest = (callback: FrameRequestCallback) => number;
type AnimationFrameCancel = (handle: number) => void;

function requestFrame(callback: FrameRequestCallback): number {
  if (typeof requestAnimationFrame === "function") {
    return requestAnimationFrame(callback);
  }
  return setTimeout(() => callback(Date.now()), 16) as unknown as number;
}

function cancelFrame(handle: number): void {
  if (typeof cancelAnimationFrame === "function") {
    cancelAnimationFrame(handle);
    return;
  }
  clearTimeout(handle as unknown as ReturnType<typeof setTimeout>);
}

export function createScrollFrameScheduler(
  run: () => void,
  options: {
    requestAnimationFrame?: AnimationFrameRequest;
    cancelAnimationFrame?: AnimationFrameCancel;
  } = {},
) {
  const request = options.requestAnimationFrame ?? requestFrame;
  const cancel = options.cancelAnimationFrame ?? cancelFrame;
  let frame: number | null = null;
  let disconnected = false;

  function cancelPending() {
    if (frame === null) return;
    cancel(frame);
    frame = null;
  }

  function schedule() {
    if (disconnected || frame !== null) return;
    frame = request(() => {
      frame = null;
      if (!disconnected) run();
    });
  }

  function flush() {
    if (disconnected) return;
    cancelPending();
    run();
  }

  function disconnect() {
    disconnected = true;
    cancelPending();
  }

  return { schedule, flush, cancel: cancelPending, disconnect };
}
