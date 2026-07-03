import { useStore } from "@nanostores/vue";
import { computed, getCurrentInstance, onBeforeUnmount, ref, watch, type Ref } from "vue";
import {
  $desktopToolbarOwnerId,
  $desktopToolbarSuppressed,
  $desktopToolbarSuspensionEpoch,
  clearDesktopToolbarOwner,
} from "@/stores/message-toolbar";

/**
 * Transient action surfaces for one message card: the desktop hover
 * toolbar's emoji-picker popover and the touch action sheet.
 *
 * Owns the mutual-exclusion contract between the two (never more than one
 * emoji rail on screen), the desktop-toolbar lock shared across cards via
 * the `message-toolbar` nanostores, the Escape-to-close listener that is
 * only attached while a surface is open, and the timeline-wide suspension
 * epoch that force-closes everything (e.g. when a modal opens).
 */
export function useMessageActionSurfaces(input: {
  messageId: () => string;
  reactionModeSelected: () => boolean;
  /** Message-card root; used to blur toolbar focus left behind on close. */
  bubbleEl: Ref<HTMLElement | null>;
}) {
  // Inline hover toolbar's SmilePlus popover. Desktop-only; bound to hover.
  const pickerOpen = ref(false);
  // Unified action sheet: touch long-press and the mobile MoreHorizontal trigger
  // open the same surface so there is never more than one emoji rail on screen.
  const sheetOpen = ref(false);

  const desktopToolbarOwnerId = useStore($desktopToolbarOwnerId);
  const desktopToolbarSuppressed = useStore($desktopToolbarSuppressed);
  const desktopToolbarSuspensionEpoch = useStore($desktopToolbarSuspensionEpoch);
  const ownsDesktopToolbarLock = computed(() => desktopToolbarOwnerId.value === input.messageId());
  const desktopToolbarLockedByAnother = computed(() =>
    desktopToolbarOwnerId.value !== null && desktopToolbarOwnerId.value !== input.messageId(),
  );
  const desktopToolbarVisibilityClass = computed(() => {
    if (desktopToolbarSuppressed.value) {
      return "opacity-0 motion-safe:translate-y-1 pointer-events-none z-sticky";
    }
    if (ownsDesktopToolbarLock.value || input.reactionModeSelected()) {
      return "opacity-100 translate-y-0 pointer-events-auto z-floating";
    }
    return "opacity-0 motion-safe:translate-y-1 group-hover:opacity-100 group-hover:translate-y-0 focus-within:opacity-100 focus-within:translate-y-0 pointer-events-none group-hover:pointer-events-auto focus-within:pointer-events-auto z-sticky";
  });
  const anyOverlayOpen = computed(() => pickerOpen.value || sheetOpen.value);

  function blurToolbarFocus() {
    if (typeof document === "undefined") return;
    const active = document.activeElement;
    if (!(active instanceof HTMLElement)) return;
    if (!input.bubbleEl.value?.contains(active)) return;
    active.blur();
  }

  function closePicker(blur = false) {
    if (ownsDesktopToolbarLock.value) $desktopToolbarOwnerId.set(null);
    pickerOpen.value = false;
    if (blur) blurToolbarFocus();
  }

  function closeSheet() {
    sheetOpen.value = false;
  }

  function closeTransientSurfaces() {
    pickerOpen.value = false;
    closeSheet();
    if (ownsDesktopToolbarLock.value) clearDesktopToolbarOwner();
    blurToolbarFocus();
  }

  function openSheet() {
    closePicker();
    sheetOpen.value = true;
  }

  function togglePicker() {
    const next = !pickerOpen.value;
    closeSheet();
    if (next) {
      $desktopToolbarOwnerId.set(input.messageId());
      pickerOpen.value = true;
    }
    else closePicker(true);
  }

  function onWindowKeydown(event: KeyboardEvent) {
    if (event.key !== "Escape") return;
    if (sheetOpen.value) closeSheet();
    else closePicker(true);
  }

  // Only listen globally while an overlay is actually open so a long timeline
  // does not attach a handler per card. Outside-click closing for the picker is
  // owned by EmojiPicker itself (its panel is teleported to <body>, so a
  // bubble-relative check here would close it on every in-panel click); the
  // action sheet uses a backdrop. We only handle Escape here.
  watch(
    anyOverlayOpen,
    (open) => {
      if (typeof window === "undefined") return;
      if (open) window.addEventListener("keydown", onWindowKeydown);
      else window.removeEventListener("keydown", onWindowKeydown);
    },
  );

  watch(
    () => desktopToolbarSuspensionEpoch.value,
    () => {
      closeTransientSurfaces();
    },
  );

  watch(
    () => desktopToolbarOwnerId.value,
    (ownerId) => {
      if (ownerId === input.messageId()) return;
      if (pickerOpen.value) pickerOpen.value = false;
    },
  );

  // Guarded so the composable is exercisable from a bare `effectScope`
  // in tests (same pattern as `shell/tab-unread-indicator`).
  if (getCurrentInstance()) {
    onBeforeUnmount(() => {
      if (ownsDesktopToolbarLock.value) $desktopToolbarOwnerId.set(null);
    });

    onBeforeUnmount(() => {
      if (typeof window === "undefined") return;
      window.removeEventListener("keydown", onWindowKeydown);
    });
  }

  return {
    pickerOpen,
    sheetOpen,
    desktopToolbarLockedByAnother,
    desktopToolbarVisibilityClass,
    closePicker,
    closeSheet,
    openSheet,
    togglePicker,
  };
}
