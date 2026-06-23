/**
 * Pure keyboard-shortcut mapping for the in-call surfaces (#1034).
 *
 * The single source of truth for "which key does what" lives here as a
 * total function from a keyboard-event descriptor to a typed intent — no
 * DOM, no side effects — so the mapping can be exhaustively unit-tested
 * and the Vue wiring stays a thin adapter that only translates DOM events
 * into descriptors and dispatches the resulting intent to existing call
 * actions.
 */

export type CallShortcutIntent =
  | "toggle-mic"
  | "push-to-talk-start"
  | "push-to-talk-end"
  | "toggle-camera"
  | "toggle-share"
  | "toggle-raise-hand"
  | "enter-immersive"
  | "exit-immersive";

/**
 * Everything the mapping needs to decide an intent, lifted out of the DOM
 * `KeyboardEvent` so the decision is pure and testable. `surfaceFocused`
 * (focus within a call surface) and `editableTarget` (typing into an
 * input / contenteditable) are computed once by the wiring adapter.
 */
export type CallShortcutInput = {
  key: string;
  type: "keydown" | "keyup";
  repeat: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  editableTarget: boolean;
  /**
   * The focused element activates on Space/Enter (a button, link, menu item,
   * …). Space must pass through to that native activation rather than being
   * stolen for push-to-talk.
   */
  activationTarget: boolean;
  surfaceFocused: boolean;
};

/**
 * Roots of the in-call surfaces that live in the main document — split
 * (inline above chat) and expanded/immersive. Focus within either scopes the
 * shortcuts to "a focused call surface". The document Picture-in-Picture
 * window is intentionally excluded: it is a separate `Window`, so its key
 * events never reach this window's listener — it owns its own dismissal.
 */
const CALL_SURFACE_SELECTOR = ".call-split, .call-expanded";

/**
 * Text-entry targets where the keys must stay inert. Matches the editable
 * guard used elsewhere in the chat shell (`isEditableKeyTarget`) so typing
 * in the composer, a search box, or a settings field never toggles the call.
 */
const EDITABLE_SELECTOR = "input, textarea, select, [contenteditable='true']";

/**
 * Controls whose default keyboard action is Space/Enter activation. When one
 * is focused, Space belongs to the control (e.g. activating a focused Mute or
 * Hang-up button), not to push-to-talk.
 */
const ACTIVATION_SELECTOR =
  'button, a[href], summary, [role="button"], [role="menuitem"], ' +
  '[role="menuitemcheckbox"], [role="menuitemradio"], [role="tab"], ' +
  '[role="switch"], [role="option"]';

/** The minimal slice of `Element` the target descriptor needs. */
type ClosestQueryable = Pick<Element, "closest">;

/**
 * Derive the scope booleans `resolveCallShortcut` consumes from the keyboard
 * event's target. Pure: it only reads `closest`, so the wiring adapter narrows
 * `event.target` to an element and hands it here.
 */
export function describeCallShortcutTarget(
  target: ClosestQueryable | null,
): { surfaceFocused: boolean; editableTarget: boolean; activationTarget: boolean } {
  if (!target) {
    return { surfaceFocused: false, editableTarget: false, activationTarget: false };
  }
  return {
    surfaceFocused: target.closest(CALL_SURFACE_SELECTOR) !== null,
    editableTarget: target.closest(EDITABLE_SELECTOR) !== null,
    activationTarget: target.closest(ACTIVATION_SELECTOR) !== null,
  };
}

export function resolveCallShortcut(input: CallShortcutInput): CallShortcutIntent | null {
  // Scoped to a focused call surface, and inert while the user is typing in
  // chat — both checked before anything else so no key ever leaks through.
  if (!input.surfaceFocused) return null;
  if (input.editableTarget) return null;
  // Never hijack OS / browser chords (Cmd+M, Ctrl+S, …) — only bare keys.
  if (input.ctrlKey || input.metaKey || input.altKey) return null;

  if (input.key === " ") {
    // A focused button/link/menu item owns Space for its own activation — let
    // it through rather than stealing it (and suppressing it) for push-to-talk.
    if (input.activationTarget) return null;
    if (input.type === "keyup") return "push-to-talk-end";
    // Auto-repeat would spam push-to-talk-start while the key is held.
    return input.repeat ? null : "push-to-talk-start";
  }
  // Toggles and one-shots fire on the initial keydown only — keyup and
  // OS auto-repeat must not re-fire them.
  if (input.type === "keyup" || input.repeat) return null;
  switch (input.key.toLowerCase()) {
    case "m":
      return "toggle-mic";
    case "v":
      return "toggle-camera";
    case "s":
      return "toggle-share";
    case "r":
      return "toggle-raise-hand";
    case "f":
      return "enter-immersive";
    case "escape":
      return "exit-immersive";
    default:
      return null;
  }
}
