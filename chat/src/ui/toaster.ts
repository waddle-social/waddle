import { createToaster } from "@ark-ui/vue/toast";

/**
 * The app-wide toast store (Ark Toast). `AppToaster.vue` renders it once
 * near the root; anything else calls `toast()` to show a notice.
 *
 * Bottom-end, at most four visible, 12px apart. Toasts stay
 * night-coloured in daylight (see the `toast` recipe).
 */
export const toaster = createToaster({
  placement: "bottom-end",
  gap: 12,
  max: 4,
  overlap: false,
});

/**
 * `live` is ember: something happening right now (a call, a mention).
 * `neutral` is a plain notice. `danger` is a failure.
 */
type ToastTone = "live" | "neutral" | "danger";

export interface ToastOptions {
  title: string;
  description?: string;
  action?: { label: string; onClick: () => void };
  tone?: ToastTone;
  /** Milliseconds before auto-dismiss; `Infinity` keeps it until dismissed. */
  duration?: number;
  /** Stable id so a later `toast()` with the same id updates in place. */
  id?: string;
}

/** Waddle tone carried on the toast; `AppToaster.vue` reads it back. */
export interface ToastMeta {
  tone: ToastTone;
}

/**
 * zag's toast store only knows error/warning/loading/success/info and
 * looks the queue priority up by `type`, so a Waddle tone must never be
 * the `type`: `danger` rides on `error`, everything else on `info`, and
 * the tone itself travels in `meta`.
 */
function zagTypeFor(tone: ToastTone): "error" | "info" {
  return tone === "danger" ? "error" : "info";
}

/**
 * Show a toast and return its id. Re-using an `id` updates that toast.
 * Only set fields are passed on: zag spreads the payload over its
 * defaults, so an explicit `undefined` would erase the generated id or
 * the default duration.
 */
export function toast(options: ToastOptions): string {
  const tone = options.tone ?? "neutral";
  const meta: ToastMeta = { tone };
  const data = {
    title: options.title,
    type: zagTypeFor(tone),
    meta,
    ...(options.id !== undefined ? { id: options.id } : {}),
    ...(options.description !== undefined ? { description: options.description } : {}),
    ...(options.action !== undefined ? { action: options.action } : {}),
    ...(options.duration !== undefined ? { duration: options.duration } : {}),
  };
  if (options.id && toaster.isVisible(options.id)) {
    return toaster.update(options.id, data);
  }
  return toaster.create(data);
}

toast.dismiss = (id?: string): void => toaster.dismiss(id);
