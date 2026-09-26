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

/** Show a toast and return its id. Re-using an `id` updates that toast. */
export function toast(options: ToastOptions): string {
  const data = {
    id: options.id,
    title: options.title,
    description: options.description,
    action: options.action,
    duration: options.duration,
    type: options.tone ?? "neutral",
  };
  if (options.id && toaster.isVisible(options.id)) {
    return toaster.update(options.id, data);
  }
  return toaster.create(data);
}

toast.dismiss = (id?: string): void => toaster.dismiss(id);
