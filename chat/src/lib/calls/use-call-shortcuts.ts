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
  if (target === null) return null;
  const candidate = target as { closest?: unknown };
  return typeof candidate.closest === "function" ? (candidate as ClosestQueryable) : null;
}

/**
 * Install capture-phase keydown/keyup listeners that resolve and dispatch
 * call shortcuts. Returns a teardown that removes the listeners. Capture
 * phase keeps the editable-target guard authoritative even when an inner
 * widget would otherwise stop propagation.
 *
 * `isActive` gates dispatch: both call surfaces stay mounted at once, so each
 * passes a predicate for "am I the visible surface?" — without it every
 * shortcut would fire twice. Defaults to always-active for single-surface use.
 *
 * Push-to-talk release is handled out-of-band from the pure mapping: once a
 * hold has engaged the mic, the release must ALWAYS fire — even if focus moved
 * into the chat composer mid-hold (the Space keyup still bubbles to the window)
 * or the window lost focus entirely (alt-tab) — otherwise the mic sticks open.
 * So the Space keyup, a window `blur`, and teardown all force a release while
 * engaged, regardless of the focus/active scoping that gates the press.
 */
export function installCallShortcuts(
  handlers: CallShortcutHandlers,
  target: ShortcutListenerTarget,
  isActive: () => boolean = () => true,
): () => void {
  let pushToTalkEngaged = false;

  const releasePushToTalk = (): void => {
    if (!pushToTalkEngaged) return;
    pushToTalkEngaged = false;
    handlers["push-to-talk-end"]?.();
  };

  const onKey = (event: Event): void => {
    const keyboardEvent = event as KeyboardEvent;
    if (keyboardEvent.type !== "keydown" && keyboardEvent.type !== "keyup") return;

    // Release safety net first, before the focus/active scoping that gates the
    // press: a held mic must come back down on release no matter where focus
    // went. A Space keyup with no engaged hold is left untouched.
    if (keyboardEvent.type === "keyup" && keyboardEvent.key === " ") {
      if (!pushToTalkEngaged) return;
      releasePushToTalk();
      keyboardEvent.preventDefault();
      return;
    }

    if (!isActive()) return;
    const { surfaceFocused, editableTarget, activationTarget } = describeCallShortcutTarget(
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
      activationTarget,
      surfaceFocused,
    });
    if (intent === null) return;
    const handler = handlers[intent];
    if (handler === undefined) return;
    // A handler that returns false declined to act — leave the key alone.
    if (handler() === false) return;
    if (intent === "push-to-talk-start") pushToTalkEngaged = true;
    keyboardEvent.preventDefault();
  };

  const onBlur = (): void => releasePushToTalk();

  target.addEventListener("keydown", onKey, true);
  target.addEventListener("keyup", onKey, true);
  target.addEventListener("blur", onBlur);
  return () => {
    target.removeEventListener("keydown", onKey, true);
    target.removeEventListener("keyup", onKey, true);
    target.removeEventListener("blur", onBlur);
    releasePushToTalk();
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
