import { onBeforeUnmount, onMounted } from "vue";
import {
  describeCallShortcutTarget,
  resolveCallShortcut,
  type CallShortcutIntent,
} from "./call-shortcuts";

/**
 * Thin DOM/Vue adapter over the pure `call-shortcuts` mapping (#1034).
 *
 * The decision of "which key does what" lives in `resolveCallShortcut`; this
 * module only translates real keyboard events into the descriptor that
 * mapping consumes and dispatches the resulting intent to the handler a call
 * surface supplies. A surface registers handlers only for the intents it
 * owns — an unmapped key (e.g. Escape on the expanded surface, which has its
 * own richer handler) is left untouched so existing listeners still run.
 *
 * A handler may return `false` to signal "I chose not to act" (e.g. Escape
 * when no Picture-in-Picture is open), leaving the keystroke for other
 * listeners; any other return value consumes it.
 */
export type CallShortcutHandlers = Partial<Record<CallShortcutIntent, () => boolean | void>>;

type ShortcutListenerTarget = Pick<Window, "addEventListener" | "removeEventListener">;

/** The minimal slice of an element the target descriptor needs. */
type ClosestQueryable = { closest(selectors: string): Element | null };

function closestQueryable(target: EventTarget | null): ClosestQueryable | null {
  return target !== null &&
    typeof (target as { closest?: unknown }).closest === "function"
    ? (target as ClosestQueryable)
    : null;
}

/**
 * Install capture-phase keydown/keyup listeners that resolve and dispatch
 * call shortcuts. Returns a teardown that removes both listeners. Capture
 * phase keeps the editable-target guard authoritative even when an inner
 * widget would otherwise stop propagation.
 *
 * `isActive` gates dispatch: both call surfaces stay mounted at once, so each
 * passes a predicate for "am I the visible surface?" — without it every
 * shortcut would fire twice. Defaults to always-active for single-surface use.
 */
export function installCallShortcuts(
  handlers: CallShortcutHandlers,
  target: ShortcutListenerTarget,
  isActive: () => boolean = () => true,
): () => void {
  const onKey = (event: Event): void => {
    const keyboardEvent = event as KeyboardEvent;
    if (keyboardEvent.type !== "keydown" && keyboardEvent.type !== "keyup") return;
    if (!isActive()) return;
    const { surfaceFocused, editableTarget } = describeCallShortcutTarget(
      closestQueryable(keyboardEvent.target),
    );
    const intent = resolveCallShortcut({
      key: keyboardEvent.key,
      type: keyboardEvent.type,
      repeat: keyboardEvent.repeat,
      ctrlKey: keyboardEvent.ctrlKey,
      metaKey: keyboardEvent.metaKey,
      altKey: keyboardEvent.altKey,
      editableTarget,
      surfaceFocused,
    });
    if (intent === null) return;
    const handler = handlers[intent];
    if (handler === undefined) return;
    // A handler that returns false declined to act — leave the key alone.
    if (handler() === false) return;
    keyboardEvent.preventDefault();
  };

  target.addEventListener("keydown", onKey, true);
  target.addEventListener("keyup", onKey, true);
  return () => {
    target.removeEventListener("keydown", onKey, true);
    target.removeEventListener("keyup", onKey, true);
  };
}

/**
 * Vue lifecycle wrapper: install the shortcuts on `window` for the lifetime
 * of the calling component (a mounted call surface) and tear them down on
 * unmount. A no-op during SSR. `isActive` lets a surface that is mounted but
 * not currently visible stand down so shortcuts never fire twice.
 */
export function useCallShortcuts(
  handlers: CallShortcutHandlers,
  isActive: () => boolean = () => true,
): void {
  if (typeof window === "undefined") return;
  let teardown: (() => void) | null = null;
  onMounted(() => {
    teardown = installCallShortcuts(handlers, window, isActive);
  });
  onBeforeUnmount(() => {
    teardown?.();
    teardown = null;
  });
}
