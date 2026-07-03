import { describe, expect, mock, test } from "bun:test";
import { effectScope, nextTick } from "vue";
import {
  useThreadComposer,
  type ThreadReplyTarget,
  type ThreadSendOverride,
} from "../src/components/chat/composables/use-thread-composer";
import type { TimelineMessage } from "../src/lib/chat-ui";

function message(partial: Partial<TimelineMessage> & { id: string }): TimelineMessage {
  return {
    author: "alice",
    body: "",
    createdAt: "2026-01-01T00:00:00Z",
    isSelf: false,
    ...partial,
  };
}

type Composer = ReturnType<typeof useThreadComposer>;

function makeHarness(options: {
  activeThreadId?: string | null;
  parentThreadId?: string | undefined;
  rootMessage?: TimelineMessage | null;
  hideComposer?: boolean;
} = {}) {
  const emitSend = mock(
    (
      _body: string,
      _markup: unknown,
      _references: unknown,
      _files: unknown,
      _replyTo: ThreadReplyTarget | undefined,
      _override: ThreadSendOverride,
    ) => {},
  );
  const emitSelectGif = mock((_url: string, _override: ThreadSendOverride) => {});
  const emitTyping = mock((_override?: ThreadSendOverride) => {});
  const scope = effectScope();
  let composer: Composer | undefined;
  scope.run(() => {
    composer = useThreadComposer({
      activeThreadId: () =>
        options.activeThreadId === undefined ? "thread-1" : options.activeThreadId,
      parentThreadId: () => options.parentThreadId,
      rootMessage: () => options.rootMessage ?? null,
      hideComposer: () => options.hideComposer ?? false,
      emitSend,
      emitSelectGif,
      emitTyping,
    });
  });
  if (!composer) throw new Error("composable did not initialize");
  return { composer, emitSend, emitSelectGif, emitTyping, stop: () => scope.stop() };
}

describe("useThreadComposer", () => {
  test("onSend defaults the reply target to the thread root (preferring the JID)", () => {
    const root = message({ id: "root-1", author: "alice", authorJid: "alice@waddle.example", body: "root body" });
    const { composer, emitSend, stop } = makeHarness({ rootMessage: root });
    composer.draft.value = "hello";
    composer.onSend("hello", [], []);
    expect(emitSend).toHaveBeenCalledTimes(1);
    expect(emitSend.mock.calls[0]?.[4]).toEqual({
      id: "root-1",
      author: "alice@waddle.example",
      body: "root body",
    });
    expect(emitSend.mock.calls[0]?.[5]).toEqual({ threadId: "thread-1" });
    expect(composer.draft.value).toBe("");
    stop();
  });

  test("an explicit in-thread reply overrides the root fallback and clears afterwards", () => {
    const root = message({ id: "root-1", author: "alice" });
    const { composer, emitSend, stop } = makeHarness({ rootMessage: root });
    composer.beginReplyInThread(message({ id: "reply-1", author: "bob", body: "reply body" }));
    expect(composer.replyingTo.value).toEqual({ id: "reply-1", author: "bob", body: "reply body" });
    composer.onSend("hi", [], []);
    expect(emitSend.mock.calls[0]?.[4]).toEqual({ id: "reply-1", author: "bob", body: "reply body" });
    expect(composer.replyingTo.value).toBeNull();
    stop();
  });

  test("nested threads stamp the parent thread on the override", () => {
    const { composer, emitSend, emitSelectGif, emitTyping, stop } = makeHarness({
      parentThreadId: "parent-thread",
    });
    composer.onSend("hi", [], []);
    composer.onSelectGif("https://gif.example/g.gif");
    composer.onTyping();
    const expected = { threadId: "thread-1", parentThreadId: "parent-thread" };
    expect(emitSend.mock.calls[0]?.[5]).toEqual(expected);
    expect(emitSelectGif.mock.calls[0]?.[1]).toEqual(expected);
    expect(emitTyping.mock.calls[0]?.[0]).toEqual(expected);
    stop();
  });

  test("without an active thread nothing is sent and typing is unscoped", () => {
    const { composer, emitSend, emitSelectGif, emitTyping, stop } = makeHarness({
      activeThreadId: null,
    });
    composer.onSend("hi", [], []);
    composer.onSelectGif("https://gif.example/g.gif");
    composer.onTyping();
    expect(emitSend).not.toHaveBeenCalled();
    expect(emitSelectGif).not.toHaveBeenCalled();
    expect(emitTyping).toHaveBeenCalledTimes(1);
    expect(emitTyping.mock.calls[0]?.[0]).toBeUndefined();
    stop();
  });

  test("beginReplyInThread is inert in the hidden-composer context pane", () => {
    const { composer, stop } = makeHarness({ hideComposer: true });
    composer.beginReplyInThread(message({ id: "reply-1", author: "bob" }));
    expect(composer.replyingTo.value).toBeNull();
    stop();
  });

  test("beginReplyInThread focuses the composer on the next tick", async () => {
    const { composer, stop } = makeHarness();
    const focus = mock(() => {});
    composer.setComposerRef({ focus });
    composer.beginReplyInThread(message({ id: "reply-1", author: "bob" }));
    await nextTick();
    expect(focus).toHaveBeenCalledTimes(1);
    stop();
  });

  test("cancel and thread-switch reset discard the pending reply and draft", () => {
    const { composer, stop } = makeHarness();
    composer.beginReplyInThread(message({ id: "reply-1", author: "bob" }));
    composer.cancelReplyInThread();
    expect(composer.replyingTo.value).toBeNull();

    composer.beginReplyInThread(message({ id: "reply-2", author: "bob" }));
    composer.draft.value = "unsent";
    composer.resetForThreadSwitch();
    expect(composer.replyingTo.value).toBeNull();
    expect(composer.draft.value).toBe("");
    stop();
  });
});
