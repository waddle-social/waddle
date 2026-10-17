// Touch "tap a post to open its thread". Desktop keeps text selection
// and uses the reply card; on a coarse pointer a plain tap anywhere on a
// post with replies opens the thread, unless the tap belongs to
// something else on the row.

/**
 * Elements that own their own tap: links, buttons, form fields, media,
 * the XEP-0461 reply quote and avatar/name (both buttons), reactions
 * and chips (buttons), and anything opted out with `data-row-tap-ignore`.
 */
const INTERACTIVE_TAP_TARGET_SELECTOR = [
  "a[href]",
  "button",
  "input",
  "textarea",
  "select",
  "label",
  "summary",
  "img",
  "video",
  "audio",
  "iframe",
  "canvas",
  "[role='button']",
  "[role='link']",
  "[contenteditable='true']",
  "[data-row-tap-ignore]",
].join(", ");

interface ClosestCapable {
  closest(selector: string): unknown;
}

interface ContainsCapable {
  contains(other: unknown): boolean;
}

function canClosest(value: unknown): value is ClosestCapable {
  return !!value && typeof (value as ClosestCapable).closest === "function";
}

/**
 * Whether a tap on `target` lands on an interactive child of `row`.
 * Only matches inside the row (never the row itself or its ancestors),
 * so a timeline wrapper with a role or tabindex does not count.
 */
export function tapTargetIsInteractive(target: unknown, row: ContainsCapable): boolean {
  // Text-node targets (rare for clicks) resolve through their parent.
  const element = canClosest(target)
    ? target
    : canClosest((target as { parentElement?: unknown } | null)?.parentElement)
      ? (target as { parentElement: ClosestCapable }).parentElement
      : null;
  if (!element) return false;
  const hit = element.closest(INTERACTIVE_TAP_TARGET_SELECTOR);
  return !!hit && hit !== row && row.contains(hit);
}

export interface RowTapInput {
  /** The row shows a reply card (has replies, not inside the thread pane). */
  opensThread: boolean;
  /** The device's primary pointer is coarse (`(pointer: coarse)`). */
  coarsePointer: boolean;
  /** The click came from a touch press that stayed put (no swipe, no long press). */
  stationaryTouch: boolean;
  /** A non-collapsed text selection exists. */
  hasTextSelection: boolean;
  /** The tap landed on an interactive child of the row. */
  targetIsInteractive: boolean;
}

export function shouldOpenThreadOnRowTap(input: RowTapInput): boolean {
  return input.opensThread
    && input.coarsePointer
    && input.stationaryTouch
    && !input.hasTextSelection
    && !input.targetIsInteractive;
}

export function prefersCoarsePointer(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
  return window.matchMedia("(pointer: coarse)").matches;
}

export function hasActiveTextSelection(): boolean {
  if (typeof document === "undefined" || typeof document.getSelection !== "function") return false;
  const selection = document.getSelection();
  return !!selection && !selection.isCollapsed && selection.toString().length > 0;
}
