import { describe, expect, mock, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import { useThreadOlderLoadRestore } from "../src/components/chat/composables/use-thread-older-load-restore";
import type { ScrollDirectionMode } from "../src/lib/scroll-direction";

interface FakeScroller {
  scrollHeight: number;
  scrollTop: number;
}

function makeHarness(options: { mode?: ScrollDirectionMode } = {}) {
  const element: FakeScroller = { scrollHeight: 1000, scrollTop: 100 };
  const scrollContainer = ref<HTMLElement | null>(element as unknown as HTMLElement);
  const isLoadingOlderReplies = ref<boolean | undefined>(false);
  const mode = ref<ScrollDirectionMode>(options.mode ?? "chat");
  const activeThreadId = ref<string | null>("thread-1");
  const cancelSettleLock = mock(() => {});
  const afterRestore = mock(() => {});

  const scope = effectScope();
  scope.run(() => {
    useThreadOlderLoadRestore({
      isLoadingOlderReplies: () => isLoadingOlderReplies.value,
      scrollContainer,
      mode: () => mode.value,
      activeThreadId: () => activeThreadId.value,
      cancelSettleLock,
      afterRestore,
    });
  });
  return {
    element,
    scrollContainer,
    isLoadingOlderReplies,
    mode,
    activeThreadId,
    cancelSettleLock,
    afterRestore,
    stop: () => scope.stop(),
  };
}

async function settle() {
  await nextTick();
  await nextTick();
}

describe("useThreadOlderLoadRestore", () => {
  test("keeps the reader's place after older replies prepend in chat mode", async () => {
    const h = makeHarness();
    h.isLoadingOlderReplies.value = true;
    await nextTick();
    expect(h.cancelSettleLock).toHaveBeenCalledTimes(1);

    // Older page inserted above: the scroll height grows by 400.
    h.element.scrollHeight = 1400;
    h.isLoadingOlderReplies.value = false;
    await settle();
    expect(h.element.scrollTop).toBe(500);
    expect(h.afterRestore).toHaveBeenCalledTimes(1);
    h.stop();
  });

  test("does not adjust in top-pinned social mode", async () => {
    const h = makeHarness({ mode: "social" });
    h.isLoadingOlderReplies.value = true;
    await nextTick();
    h.element.scrollHeight = 1400;
    h.isLoadingOlderReplies.value = false;
    await settle();
    expect(h.element.scrollTop).toBe(100);
    expect(h.afterRestore).toHaveBeenCalledTimes(1);
    h.stop();
  });

  test("abandons the snapshot when the thread changes mid-load", async () => {
    const h = makeHarness();
    h.isLoadingOlderReplies.value = true;
    await nextTick();
    h.activeThreadId.value = "thread-2";
    h.element.scrollHeight = 1400;
    h.isLoadingOlderReplies.value = false;
    await settle();
    expect(h.element.scrollTop).toBe(100);
    expect(h.afterRestore).toHaveBeenCalledTimes(1);
    h.stop();
  });

  test("abandons the snapshot when the scroll mode flips mid-load", async () => {
    const h = makeHarness();
    h.isLoadingOlderReplies.value = true;
    await nextTick();
    h.mode.value = "social";
    h.element.scrollHeight = 1400;
    h.isLoadingOlderReplies.value = false;
    await settle();
    expect(h.element.scrollTop).toBe(100);
    expect(h.afterRestore).toHaveBeenCalledTimes(1);
    h.stop();
  });

  test("does nothing when finishing without a snapshot", async () => {
    const h = makeHarness();
    h.scrollContainer.value = null;
    h.isLoadingOlderReplies.value = true;
    await nextTick();
    h.isLoadingOlderReplies.value = false;
    await settle();
    expect(h.afterRestore).not.toHaveBeenCalled();
    h.stop();
  });
});
