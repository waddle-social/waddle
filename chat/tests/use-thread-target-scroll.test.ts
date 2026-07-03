import { describe, expect, mock, test } from "bun:test";
import { effectScope, nextTick, ref } from "vue";
import { useThreadTargetScroll } from "../src/components/chat/composables/use-thread-target-scroll";
import type { TimelineMessage } from "../src/lib/chat-ui";

function message(id: string): TimelineMessage {
  return {
    id,
    author: "alice",
    body: "",
    createdAt: "2026-01-01T00:00:00Z",
    isSelf: false,
  };
}

function makeHarness(options: {
  targetMessageId?: string | null;
  scrollResult?: boolean;
  initialMessages?: TimelineMessage[];
} = {}) {
  const targetMessageId = ref<string | null>(options.targetMessageId ?? null);
  const targetMessageRequestId = ref(0);
  const activeThreadId = ref<string | null>("thread-1");
  const orderedMessages = ref<TimelineMessage[]>(options.initialMessages ?? []);
  const scrollToMessageId = mock(
    (_messageId: string, _align?: "start" | "center" | "end") =>
      Promise.resolve(options.scrollResult ?? true),
  );
  const timelineAttached = ref(true);
  const afterScroll = mock(() => {});
  const markDisplayed = mock(() => {});

  const scope = effectScope();
  let targetScroll: ReturnType<typeof useThreadTargetScroll> | undefined;
  scope.run(() => {
    targetScroll = useThreadTargetScroll({
      targetMessageId: () => targetMessageId.value,
      targetMessageRequestId: () => targetMessageRequestId.value,
      activeThreadId: () => activeThreadId.value,
      orderedMessages: () => orderedMessages.value,
      timeline: () => (timelineAttached.value ? { scrollToMessageId } : null),
      afterScroll,
      markDisplayed,
    });
  });
  if (!targetScroll) throw new Error("composable did not initialize");
  return {
    targetScroll,
    targetMessageId,
    targetMessageRequestId,
    activeThreadId,
    orderedMessages,
    timelineAttached,
    scrollToMessageId,
    afterScroll,
    markDisplayed,
    stop: () => scope.stop(),
  };
}

async function settle() {
  // scrollToTargetMessage awaits two ticks internally.
  await nextTick();
  await nextTick();
  await nextTick();
}

describe("useThreadTargetScroll", () => {
  test("starts pending when a target is set and idle otherwise", () => {
    const withTarget = makeHarness({ targetMessageId: "m-1" });
    expect(withTarget.targetScroll.targetScrollPending.value).toBe(true);
    withTarget.stop();

    const withoutTarget = makeHarness();
    expect(withoutTarget.targetScroll.targetScrollPending.value).toBe(false);
    withoutTarget.stop();
  });

  test("without a target it clears state and reports idle for displayed-markers", async () => {
    const h = makeHarness();
    await h.targetScroll.scrollToTargetMessage();
    expect(h.targetScroll.targetScrollPending.value).toBe(false);
    expect(h.markDisplayed).toHaveBeenCalledTimes(1);
    expect(h.scrollToMessageId).not.toHaveBeenCalled();
    h.stop();
  });

  test("stays pending until the target message is rendered, then scrolls once", async () => {
    const h = makeHarness({ targetMessageId: "m-2" });
    await h.targetScroll.scrollToTargetMessage();
    expect(h.targetScroll.targetScrollPending.value).toBe(true);
    expect(h.scrollToMessageId).not.toHaveBeenCalled();

    // The message arriving triggers the watcher via the ordered-ids key.
    h.orderedMessages.value = [message("m-1"), message("m-2")];
    await settle();
    expect(h.scrollToMessageId).toHaveBeenCalledTimes(1);
    expect(h.scrollToMessageId.mock.calls[0]).toEqual(["m-2", "center"]);
    expect(h.afterScroll).toHaveBeenCalledTimes(1);
    expect(h.markDisplayed).toHaveBeenCalledTimes(1);
    expect(h.targetScroll.targetScrollPending.value).toBe(false);

    // Same (thread, message, request) key never re-scrolls.
    await h.targetScroll.scrollToTargetMessage();
    expect(h.scrollToMessageId).toHaveBeenCalledTimes(1);
    h.stop();
  });

  test("a new request id re-runs the scroll for the same message", async () => {
    const h = makeHarness({ targetMessageId: "m-1" });
    h.orderedMessages.value = [message("m-1")];
    await settle();
    expect(h.scrollToMessageId).toHaveBeenCalledTimes(1);

    h.targetMessageRequestId.value++;
    await settle();
    expect(h.scrollToMessageId).toHaveBeenCalledTimes(2);
    h.stop();
  });

  test("stays pending while the timeline is unmounted", async () => {
    const h = makeHarness({ targetMessageId: "m-1" });
    h.timelineAttached.value = false;
    h.orderedMessages.value = [message("m-1")];
    await settle();
    expect(h.scrollToMessageId).not.toHaveBeenCalled();
    expect(h.targetScroll.targetScrollPending.value).toBe(true);
    h.stop();
  });

  test("a failed scroll clears pending without completing the key", async () => {
    const h = makeHarness({ targetMessageId: "m-1", scrollResult: false });
    h.orderedMessages.value = [message("m-1")];
    await settle();
    expect(h.scrollToMessageId).toHaveBeenCalledTimes(1);
    expect(h.targetScroll.targetScrollPending.value).toBe(false);
    expect(h.afterScroll).not.toHaveBeenCalled();

    // Not marked complete: the next attempt tries again.
    await h.targetScroll.scrollToTargetMessage();
    await settle();
    expect(h.scrollToMessageId).toHaveBeenCalledTimes(2);
    h.stop();
  });

  test("cancelPendingTargetScroll abandons an in-flight scroll", async () => {
    const h = makeHarness({ targetMessageId: "m-1", initialMessages: [message("m-1")] });
    const inFlight = h.targetScroll.scrollToTargetMessage();
    h.targetScroll.cancelPendingTargetScroll();
    await inFlight;
    await settle();
    expect(h.afterScroll).not.toHaveBeenCalled();
    expect(h.targetScroll.targetScrollPending.value).toBe(false);
    h.stop();
  });
});
