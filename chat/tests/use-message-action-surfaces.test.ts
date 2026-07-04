import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import { useMessageActionSurfaces } from "../src/components/chat/composables/use-message-action-surfaces";
import {
  $desktopToolbarOwnerId,
  $desktopToolbarSuppressed,
  $desktopToolbarSuspensionEpoch,
} from "../src/stores/message-toolbar";

type Surfaces = ReturnType<typeof useMessageActionSurfaces>;

function makeHarness(options: { messageId?: string; reactionModeSelected?: boolean } = {}) {
  const reactionModeSelected = ref(options.reactionModeSelected ?? false);
  const scope = effectScope();
  let surfaces: Surfaces | undefined;
  scope.run(() => {
    surfaces = useMessageActionSurfaces({
      messageId: () => options.messageId ?? "msg-1",
      reactionModeSelected: () => reactionModeSelected.value,
      bubbleEl: ref(null),
    });
  });
  if (!surfaces) throw new Error("composable did not initialize");
  return { surfaces, reactionModeSelected, stop: () => scope.stop() };
}

describe("useMessageActionSurfaces", () => {
  // `useStore` only subscribes (and the composable only attaches its Escape
  // listener) when a window exists, so give the suite a recording stub —
  // same pattern as tests/use-long-press.test.ts.
  const originalWindow = globalThis.window;
  const keydownListeners: Array<(event: KeyboardEvent) => void> = [];

  beforeEach(() => {
    keydownListeners.length = 0;
    globalThis.window = {
      addEventListener(type: string, listener: (event: KeyboardEvent) => void) {
        if (type === "keydown") keydownListeners.push(listener);
      },
      removeEventListener(type: string, listener: (event: KeyboardEvent) => void) {
        if (type !== "keydown") return;
        const idx = keydownListeners.indexOf(listener);
        if (idx >= 0) keydownListeners.splice(idx, 1);
      },
    } as unknown as Window & typeof globalThis;
  });

  afterEach(() => {
    globalThis.window = originalWindow;
    $desktopToolbarOwnerId.set(null);
    $desktopToolbarSuppressed.set(false);
  });

  test("togglePicker claims the desktop toolbar lock and releases it on close", () => {
    const { surfaces, stop } = makeHarness();
    surfaces.togglePicker();
    expect(surfaces.pickerOpen.value).toBe(true);
    expect($desktopToolbarOwnerId.get()).toBe("msg-1");

    surfaces.togglePicker();
    expect(surfaces.pickerOpen.value).toBe(false);
    expect($desktopToolbarOwnerId.get()).toBeNull();
    stop();
  });

  test("closePicker leaves another card's lock untouched", () => {
    const { surfaces, stop } = makeHarness();
    $desktopToolbarOwnerId.set("someone-else");
    surfaces.closePicker();
    expect($desktopToolbarOwnerId.get()).toBe("someone-else");
    stop();
  });

  test("openSheet closes the picker so only one emoji rail is on screen", () => {
    const { surfaces, stop } = makeHarness();
    surfaces.togglePicker();
    surfaces.openSheet();
    expect(surfaces.pickerOpen.value).toBe(false);
    expect(surfaces.sheetOpen.value).toBe(true);
    expect($desktopToolbarOwnerId.get()).toBeNull();
    stop();
  });

  test("losing the lock to another card closes this card's picker", async () => {
    const { surfaces, stop } = makeHarness();
    surfaces.togglePicker();
    $desktopToolbarOwnerId.set("other-card");
    await nextTick();
    expect(surfaces.pickerOpen.value).toBe(false);
    expect(surfaces.desktopToolbarLockedByAnother.value).toBe(true);
    stop();
  });

  test("a suspension epoch bump force-closes all transient surfaces", async () => {
    const { surfaces, stop } = makeHarness();
    surfaces.openSheet();
    surfaces.togglePicker();
    $desktopToolbarSuspensionEpoch.set($desktopToolbarSuspensionEpoch.get() + 1);
    await nextTick();
    expect(surfaces.pickerOpen.value).toBe(false);
    expect(surfaces.sheetOpen.value).toBe(false);
    expect($desktopToolbarOwnerId.get()).toBeNull();
    stop();
  });

  test("Escape listens only while a surface is open, closing sheet before picker", async () => {
    const { surfaces, stop } = makeHarness();
    expect(keydownListeners.length).toBe(0);

    surfaces.togglePicker();
    surfaces.openSheet();
    await nextTick();
    expect(keydownListeners.length).toBe(1);

    keydownListeners[0]?.({ key: "Escape" } as KeyboardEvent);
    expect(surfaces.sheetOpen.value).toBe(false);

    surfaces.togglePicker();
    await nextTick();
    keydownListeners[0]?.({ key: "Escape" } as KeyboardEvent);
    expect(surfaces.pickerOpen.value).toBe(false);
    expect($desktopToolbarOwnerId.get()).toBeNull();

    await nextTick();
    expect(keydownListeners.length).toBe(0);
    stop();
  });

  test("visibility class tracks suppression, lock ownership, and reaction mode", async () => {
    const { surfaces, reactionModeSelected, stop } = makeHarness();
    expect(surfaces.desktopToolbarVisibilityClass.value).toContain("group-hover:opacity-100");

    reactionModeSelected.value = true;
    expect(surfaces.desktopToolbarVisibilityClass.value).toContain("opacity-100 translate-y-0");

    reactionModeSelected.value = false;
    surfaces.togglePicker();
    expect(surfaces.desktopToolbarVisibilityClass.value).toContain("opacity-100 translate-y-0");

    $desktopToolbarSuppressed.set(true);
    await nextTick();
    expect(surfaces.desktopToolbarVisibilityClass.value).toContain("pointer-events-none z-sticky");
    stop();
  });
});
