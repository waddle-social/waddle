import { describe, expect, mock, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import { useThreadDisplayedTracking } from "../src/components/chat/composables/use-thread-displayed-tracking";

function makeHarness(overrides: {
  activeThreadId?: string | null;
  latestRemoteMessageId?: string | null;
  hideComposer?: boolean;
  targetScrollPending?: boolean;
  container?: HTMLElement | null;
  isWindowFocused?: boolean;
  isPinnedAtEdge?: boolean;
  chatKey?: string;
} = {}) {
  const activeThreadId = ref<string | null>(
    overrides.activeThreadId === undefined ? "thread-1" : overrides.activeThreadId,
  );
  const latestRemoteMessageId = ref<string | null>(
    overrides.latestRemoteMessageId === undefined ? "m-1" : overrides.latestRemoteMessageId,
  );
  const hideComposer = ref(overrides.hideComposer ?? false);
  const targetScrollPending = ref(overrides.targetScrollPending ?? false);
  const scrollContainer = ref<HTMLElement | null>(
    overrides.container === undefined ? ({} as HTMLElement) : overrides.container,
  );
  const isWindowFocused = ref(overrides.isWindowFocused ?? true);
  const isPinnedAtEdge = ref(overrides.isPinnedAtEdge ?? true);
  const chatKey = ref(overrides.chatKey ?? "room@muc.example");
  const emitDisplayed = mock((_messageId: string) => {});

  const scope = effectScope();
  let tracking: ReturnType<typeof useThreadDisplayedTracking> | undefined;
  scope.run(() => {
    tracking = useThreadDisplayedTracking({
      activeThreadId: () => activeThreadId.value,
      latestRemoteMessageId: () => latestRemoteMessageId.value,
      hideComposer: () => hideComposer.value,
      targetScrollPending: () => targetScrollPending.value,
      scrollContainer,
      isWindowFocused: () => isWindowFocused.value,
      isPinnedAtEdge: () => isPinnedAtEdge.value,
      chatKey: () => chatKey.value,
      emitDisplayed,
    });
  });
  if (!tracking) throw new Error("composable did not initialize");
  return {
    tracking,
    activeThreadId,
    latestRemoteMessageId,
    hideComposer,
    targetScrollPending,
    scrollContainer,
    isWindowFocused,
    isPinnedAtEdge,
    emitDisplayed,
    stop: () => scope.stop(),
  };
}

describe("useThreadDisplayedTracking", () => {
  test("emits immediately when the thread is visibly pinned at the edge", () => {
    const h = makeHarness();
    expect(h.emitDisplayed).toHaveBeenCalledTimes(1);
    expect(h.emitDisplayed.mock.calls[0]?.[0]).toBe("m-1");
    h.stop();
  });

  test("suppressed while hidden, pending, unfocused, unpinned, or unmounted", () => {
    for (const overrides of [
      { hideComposer: true },
      { targetScrollPending: true },
      { isWindowFocused: false },
      { isPinnedAtEdge: false },
      { container: null },
      { activeThreadId: null },
      { latestRemoteMessageId: null },
    ]) {
      const h = makeHarness(overrides);
      expect(h.emitDisplayed).not.toHaveBeenCalled();
      h.stop();
    }
  });

  test("emits once the blocking condition clears, and dedupes the same message", async () => {
    const h = makeHarness({ targetScrollPending: true });
    expect(h.emitDisplayed).not.toHaveBeenCalled();

    h.targetScrollPending.value = false;
    await nextTick();
    expect(h.emitDisplayed).toHaveBeenCalledTimes(1);

    // Re-probing the same (chat, thread, message) key is a no-op.
    h.tracking.markThreadDisplayedIfVisible();
    expect(h.emitDisplayed).toHaveBeenCalledTimes(1);

    // A newer remote message emits again.
    h.latestRemoteMessageId.value = "m-2";
    await nextTick();
    expect(h.emitDisplayed).toHaveBeenCalledTimes(2);
    expect(h.emitDisplayed.mock.calls[1]?.[0]).toBe("m-2");
    h.stop();
  });

  test("switching threads emits for the new thread's newest message", async () => {
    const h = makeHarness();
    h.activeThreadId.value = "thread-2";
    await nextTick();
    expect(h.emitDisplayed).toHaveBeenCalledTimes(2);
    h.stop();
  });
});
