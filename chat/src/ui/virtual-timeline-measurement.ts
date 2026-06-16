import {
  cancelFrame,
  createRafScheduler,
  requestFrame,
  type AnimationFrameCancel,
  type AnimationFrameRequest,
} from "./raf-scheduler";

type ListenerTarget = {
  addEventListener: (
    type: string,
    listener: EventListener,
    options?: boolean | AddEventListenerOptions,
  ) => void;
  removeEventListener: (
    type: string,
    listener: EventListener,
    options?: boolean | EventListenerOptions,
  ) => void;
};

type VisibilityDocument = ListenerTarget & {
  visibilityState?: DocumentVisibilityState;
};

const MEDIA_SETTLE_EVENTS = ["load", "loadedmetadata", "error"] as const;

export function createVirtualTimelineMeasureScheduler(
  measure: () => void,
  options: {
    requestAnimationFrame?: AnimationFrameRequest;
    cancelAnimationFrame?: AnimationFrameCancel;
  } = {},
) {
  const scheduler = createRafScheduler(measure, options);
  return { scheduleMeasure: scheduler.schedule, disconnect: scheduler.disconnect };
}

export function createVirtualTimelineElementMeasureScheduler(
  measureElement: (element: HTMLElement) => void,
  options: {
    requestAnimationFrame?: AnimationFrameRequest;
    cancelAnimationFrame?: AnimationFrameCancel;
  } = {},
) {
  const request = options.requestAnimationFrame ?? requestFrame;
  const cancel = options.cancelAnimationFrame ?? cancelFrame;
  const elements = new Set<HTMLElement>();
  let frame: number | null = null;
  let disposed = false;

  function clearPendingFrame() {
    if (frame === null) return;
    cancel(frame);
    frame = null;
  }

  function scheduleMeasure(element: HTMLElement) {
    if (disposed) return;
    elements.add(element);
    if (frame !== null) return;
    frame = request(() => {
      frame = request(() => {
        frame = null;
        if (disposed) return;
        const pending = [...elements];
        elements.clear();
        for (const measuredElement of pending) measureElement(measuredElement);
      });
    });
  }

  function disconnect() {
    disposed = true;
    elements.clear();
    clearPendingFrame();
  }

  return { scheduleMeasure, disconnect };
}

function closestMeasuredRow(scrollElement: HTMLElement, event: Event): HTMLElement | null {
  const target = event.target;
  if (!target || typeof target !== "object" || !("closest" in target)) return null;
  const closest = (target as { closest?: (selector: string) => Element | null }).closest;
  if (typeof closest !== "function") return null;
  const row = closest.call(target, "[data-index]");
  if (!row) return null;
  if (typeof scrollElement.contains === "function" && !scrollElement.contains(row)) return null;
  return row as HTMLElement;
}

export function installVirtualTimelineMeasurementRecovery(options: {
  scrollElement: HTMLElement;
  measure: () => void;
  measureElement?: (element: HTMLElement) => void;
  windowTarget?: ListenerTarget | null;
  documentTarget?: VisibilityDocument | null;
  requestAnimationFrame?: AnimationFrameRequest;
  cancelAnimationFrame?: AnimationFrameCancel;
}): () => void {
  const scheduler = createVirtualTimelineMeasureScheduler(options.measure, {
    requestAnimationFrame: options.requestAnimationFrame,
    cancelAnimationFrame: options.cancelAnimationFrame,
  });
  const elementScheduler = options.measureElement
    ? createVirtualTimelineElementMeasureScheduler(options.measureElement, {
      requestAnimationFrame: options.requestAnimationFrame,
      cancelAnimationFrame: options.cancelAnimationFrame,
    })
    : null;
  const windowTarget = options.windowTarget ?? (typeof window === "undefined" ? null : window);
  const documentTarget = options.documentTarget ?? (typeof document === "undefined" ? null : document);

  const scheduleFullMeasure: EventListener = () => {
    scheduler.scheduleMeasure();
  };
  const scheduleElementMeasure: EventListener = (event) => {
    const row = elementScheduler ? closestMeasuredRow(options.scrollElement, event) : null;
    if (row && elementScheduler) {
      elementScheduler.scheduleMeasure(row);
      return;
    }
    if (!elementScheduler) scheduler.scheduleMeasure();
  };
  const scheduleWhenVisible: EventListener = () => {
    if (documentTarget?.visibilityState && documentTarget.visibilityState !== "visible") return;
    scheduler.scheduleMeasure();
  };

  for (const eventName of MEDIA_SETTLE_EVENTS) {
    options.scrollElement.addEventListener(eventName, scheduleElementMeasure, true);
  }
  documentTarget?.addEventListener("visibilitychange", scheduleWhenVisible);
  windowTarget?.addEventListener("focus", scheduleFullMeasure);
  windowTarget?.addEventListener("pageshow", scheduleFullMeasure);

  scheduler.scheduleMeasure();

  return () => {
    for (const eventName of MEDIA_SETTLE_EVENTS) {
      options.scrollElement.removeEventListener(eventName, scheduleElementMeasure, true);
    }
    documentTarget?.removeEventListener("visibilitychange", scheduleWhenVisible);
    windowTarget?.removeEventListener("focus", scheduleFullMeasure);
    windowTarget?.removeEventListener("pageshow", scheduleFullMeasure);
    elementScheduler?.disconnect();
    scheduler.disconnect();
  };
}
