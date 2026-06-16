import { readFileSync } from "node:fs";
import { describe, expect, test } from "bun:test";
import { buildReplyChildThreadTargets, resolveReplyChildThreadTarget } from "../src/lib/thread-child-target";
import { resolveThreadActionTarget } from "../src/lib/thread-action-target";
import { nextThreadStack, sameThreadStack } from "../src/lib/thread-stack";
import type { MessageThreadEntry, MessageThreadIndex } from "../src/channels/threads";
import type { TimelineMessage } from "../src/lib/chat-ui";

const messageCardSource = readFileSync(
  new URL("../src/components/chat/MessageCard.vue", import.meta.url),
  "utf8",
);
const threadPanelSource = readFileSync(
  new URL("../src/components/chat/ThreadPanel.vue", import.meta.url),
  "utf8",
);
const chatReadyShellSource = readFileSync(
  new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url),
  "utf8",
);
const chatControllerSource = readFileSync(
  new URL("../src/shell/chat-app-controller.ts", import.meta.url),
  "utf8",
);

function message(partial: Partial<TimelineMessage> & { id: string; createdAt?: string }): TimelineMessage {
  return {
    author: "alice",
    body: "",
    createdAt: "2026-01-01T00:00:00Z",
    isSelf: false,
    ...partial,
  };
}

function sourceBlock(source: string, start: string, end: string): string {
  const startIndex = source.indexOf(start);
  const endIndex = source.indexOf(end, startIndex);
  expect(startIndex).toBeGreaterThanOrEqual(0);
  expect(endIndex).toBeGreaterThan(startIndex);
  return source.slice(startIndex, endIndex);
}

function sourceBlockAfter(source: string, after: string, start: string, end: string): string {
  const afterIndex = source.indexOf(after);
  expect(afterIndex).toBeGreaterThanOrEqual(0);
  const startIndex = source.indexOf(start, afterIndex + after.length);
  const endIndex = source.indexOf(end, startIndex);
  expect(startIndex).toBeGreaterThan(afterIndex);
  expect(endIndex).toBeGreaterThan(startIndex);
  return source.slice(startIndex, endIndex);
}

describe("ThreadPanel desktop sub-thread action", () => {
  test("thread action target prefers the explicit ThreadPanel override at runtime", () => {
    expect(resolveThreadActionTarget({ id: "message-1" })).toBe("message-1");
    expect(resolveThreadActionTarget({ id: "message-1", threadId: "thread-1" })).toBe("thread-1");
    expect(resolveThreadActionTarget({ id: "message-1", threadId: "thread-1" }, "child-1")).toBe(
      "child-1",
    );
  });

  test("stack-relative thread push derives sibling and child stacks at runtime", () => {
    const baseStack = ["root"];
    const childStack = nextThreadStack(baseStack, "child-a");
    expect(childStack).toEqual(["root", "child-a"]);
    expect(childStack).not.toBe(baseStack);
    expect(baseStack).toEqual(["root"]);

    const duplicateStack = nextThreadStack(baseStack, "root");
    expect(duplicateStack).toEqual(["root"]);
    expect(duplicateStack).not.toBe(baseStack);
    expect(baseStack).toEqual(["root"]);

    expect(nextThreadStack(["root", "child-a"], "child-b")).toEqual([
      "root",
      "child-a",
      "child-b",
    ]);
    expect(sameThreadStack(["root", "child-a"], ["root", "child-a"])).toBe(true);
    expect(sameThreadStack(["root", "child-a"], ["root", "child-b"])).toBe(false);
  });

  test("reply child thread targets preserve existing arbitrary XEP-0201 child ids", () => {
    const parentReply = message({
      id: "reply-1",
      threadId: "root-thread",
      createdAt: "2026-01-01T00:01:00Z",
    });
    const arbitraryChildRoot = message({
      id: "child-root-message",
      threadId: "child-thread-id",
      parentThreadId: "root-thread",
      replyTo: { id: "reply-1", author: "alice" },
      createdAt: "2026-01-01T00:02:00Z",
    });
    const arbitraryChildReply = message({
      id: "child-reply-message",
      threadId: "child-thread-id",
      parentThreadId: "root-thread",
      replyTo: { id: "child-root-message", author: "alice" },
      createdAt: "2026-01-01T00:03:00Z",
    });
    const threadIndex: MessageThreadIndex = new Map<string, MessageThreadEntry>([
      [
        "child-thread-id",
        {
          threadId: "child-thread-id",
          parentThreadId: "root-thread",
          root: arbitraryChildRoot,
          directChildren: [arbitraryChildReply],
          allDescendants: [arbitraryChildReply],
          count: 1,
          lastTs: arbitraryChildReply.createdAt,
        },
      ],
    ]);

    const targets = buildReplyChildThreadTargets(threadIndex, "root-thread");
    expect(resolveReplyChildThreadTarget(targets, parentReply)).toEqual({
      threadId: "child-thread-id",
      count: 2,
    });
    expect(resolveReplyChildThreadTarget(targets, message({ id: "new-reply" }))).toEqual({
      threadId: "new-reply",
      count: 0,
    });
  });

  test("reply child thread targets match MUC stanza-id aliases", () => {
    const parentReply = message({
      id: "local-row-id",
      replyableId: "room-stanza-id",
      wireIds: ["origin-id", "room-stanza-id"],
      threadId: "root-thread",
      createdAt: "2026-01-01T00:01:00Z",
    });
    const arbitraryChildRoot = message({
      id: "child-root-message",
      threadId: "child-thread-id",
      parentThreadId: "root-thread",
      replyTo: { id: "room-stanza-id", author: "alice" },
      createdAt: "2026-01-01T00:02:00Z",
    });
    const threadIndex: MessageThreadIndex = new Map<string, MessageThreadEntry>([
      [
        "child-thread-id",
        {
          threadId: "child-thread-id",
          parentThreadId: "root-thread",
          root: arbitraryChildRoot,
          directChildren: [],
          allDescendants: [],
          count: 0,
          lastTs: arbitraryChildRoot.createdAt,
        },
      ],
    ]);

    const targets = buildReplyChildThreadTargets(threadIndex, "root-thread");
    expect(resolveReplyChildThreadTarget(targets, parentReply)).toEqual({
      threadId: "child-thread-id",
      count: 1,
    });
  });

  test("reply child thread targets keep implicit message-id child threads", () => {
    const parentReply = message({
      id: "reply-1",
      threadId: "root-thread",
      createdAt: "2026-01-01T00:01:00Z",
    });
    const nestedReply = message({
      id: "nested-reply",
      threadId: "reply-1",
      parentThreadId: "root-thread",
      createdAt: "2026-01-01T00:02:00Z",
    });
    const threadIndex: MessageThreadIndex = new Map<string, MessageThreadEntry>([
      [
        "reply-1",
        {
          threadId: "reply-1",
          parentThreadId: "root-thread",
          root: parentReply,
          directChildren: [nestedReply],
          allDescendants: [nestedReply],
          count: 1,
          lastTs: nestedReply.createdAt,
        },
      ],
    ]);

    const targets = buildReplyChildThreadTargets(threadIndex, "root-thread");
    expect(resolveReplyChildThreadTarget(targets, parentReply)).toEqual({
      threadId: "reply-1",
      count: 1,
    });
  });

  test("MessageCard thread action uses the explicit ThreadPanel target", () => {
    expect(messageCardSource).toContain("threadActionThreadId?: string;");
    expect(messageCardSource).toContain(
      "resolveThreadActionTarget(props.message, props.threadActionThreadId)",
    );

    const menuAction = sourceBlock(
      messageCardSource,
      "function startReplyInThreadFromMenu()",
      "const isMentioned = computed",
    );
    expect(menuAction).toContain('emit("openThread", threadActionTargetId.value)');
    expect(menuAction).not.toContain("props.message.threadId ?? props.message.id");

    const toolbarThreadButton = sourceBlockAfter(
      messageCardSource,
      '@click="startReplyFromMenu"',
      '<button\n        type="button"',
      '<button\n        v-if="canPinMessages',
    );
    expect(toolbarThreadButton).toContain('@click="startReplyInThreadFromMenu"');

    const actionSheetThreadButton = sourceBlockAfter(
      messageCardSource,
      '@click="sheetView = \'emoji\'"',
      '<button\n            type="button"',
      '<button\n            v-if="canPinMessages',
    );
    expect(actionSheetThreadButton).toContain('@click="startReplyInThreadFromMenu"');

    const swipeAction = sourceBlock(
      messageCardSource,
      "const swipe = useHorizontalSwipe({",
      "function onSwipePointerdown",
    );
    expect(swipeAction).toContain('emit("openThread", threadActionTargetId.value)');
    expect(swipeAction).not.toContain("props.message.threadId ?? props.message.id");
  });

  test("ThreadPanel targets a reply row's own child thread", () => {
    const rootCard = sourceBlock(
      threadPanelSource,
      'v-if="message.id === activeEntry.root?.id"',
      "<template v-else>",
    );
    expect(rootCard).toContain(':thread-action-thread-id="activeThreadId ?? message.id"');
    expect(rootCard).not.toContain(':thread-action-thread-id="message.id"');

    const replyCard = sourceBlockAfter(
      threadPanelSource,
      'v-if="message.id === activeEntry.root?.id"',
      '<div class="relative group/thread-child">',
      '<div v-if="replyChildHasNestedThread(message)"',
    );
    expect(replyCard).toContain(':thread-action-thread-id="replyChildThreadId(message)"');
    expect(replyCard).toContain(":thread-reply-count=\"replyChildThreadCount(message)\"");

    const childThreadAction = sourceBlock(
      threadPanelSource,
      '<div v-if="replyChildHasNestedThread(message)" class="chat-thread-actions">',
      "</button>",
    );
    expect(childThreadAction).toContain('@click="onOpenThreadFromCard(replyChildThreadId(message))"');
    expect(childThreadAction).not.toContain('onOpenThreadFromCard(message.id)');
  });

  test("parent context pane pushes relative to its displayed stack", () => {
    const parentPane = sourceBlock(
      chatReadyShellSource,
      "<!-- Parent thread pane: desktop-only context when depth >= 2.",
      "<!-- Active thread pane: shown when any thread is open.",
    );
    expect(parentPane).toContain(":thread-stack=\"activeThreadStack.slice(0, -1)\"");
    expect(parentPane).toContain(
      '@push-thread="(threadId) => pushThreadFromStack(activeThreadStack.slice(0, -1), threadId)"',
    );

    const activePane = sourceBlock(
      chatReadyShellSource,
      "<!-- Active thread pane: shown when any thread is open.",
      "</template>",
    );
    expect(activePane).toContain('@push-thread="pushThread"');
  });

  test("controller stack-relative push preserves thread backfill", () => {
    expect(chatControllerSource).toContain(
      "function pushThreadFromStack(baseStack: readonly string[], threadId: string)",
    );
    const relativePush = sourceBlock(
      chatControllerSource,
      "function pushThreadFromStack(baseStack: readonly string[], threadId: string)",
      "function pushThread(threadId: string)",
    );
    expect(relativePush).toContain("activeThreadStack.value = nextStack;");
    expect(relativePush).toContain("nextThreadStack(baseStack, threadId)");
    expect(relativePush).toContain("backfillActiveThread(threadId);");

    const defaultPush = sourceBlock(
      chatControllerSource,
      "function pushThread(threadId: string)",
      "function popThreadTo(index: number)",
    );
    expect(defaultPush).toContain("pushThreadFromStack(activeThreadStack.value, threadId);");
  });
});
