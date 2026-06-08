import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolveActiveMucCallThreadId } from "../src/lib/calls/call-chat-composer";
import type { TimelineMessage } from "../src/lib/chat-ui";

describe("call-chat composer", () => {
  test("resolves the active MUC call thread from the call anchor", () => {
    const messages: TimelineMessage[] = [
      message({ id: "ordinary", body: "hello" }),
      message({
        id: "call-anchor",
        threadId: "call-thread-123",
        roomJid: "lobby@muc.waddle.test",
        callThread: {
          kind: "muc",
          sid: "sid-active",
          media: "audio",
          initiator: "alice@waddle.test/web",
          started: "2026-06-09T12:00:00Z",
        },
      }),
      message({
        id: "other-call-anchor",
        threadId: "call-thread-older",
        roomJid: "lobby@muc.waddle.test",
        callThread: {
          kind: "muc",
          sid: "sid-old",
          media: "audio",
          initiator: "alice@waddle.test/web",
          started: "2026-06-09T11:00:00Z",
        },
      }),
    ];

    expect(resolveActiveMucCallThreadId(messages, "lobby@muc.waddle.test/alice", "sid-active")).toBe("call-thread-123");
  });

  test("expanded channel calls render a labelled composer bound to the call thread", () => {
    const source = readFileSync(
      new URL("../src/components/calls/CallExpandedSurface.vue", import.meta.url),
      "utf8",
    );

    expect(source).toContain("Call chat");
    expect(source).toContain("MessageComposer");
    expect(source).toContain("callThreadOverride");
    expect(source).toContain(":show-extensions=\"false\"");
    expect(source).toContain("emit(\"sendCallChat\"");
  });

  test("composer labels the editable control and can hide unsupported extension actions", () => {
    const composer = readFileSync(
      new URL("../src/components/chat/MessageComposer.vue", import.meta.url),
      "utf8",
    );
    const editor = readFileSync(
      new URL("../src/components/chat/ChatEditor.vue", import.meta.url),
      "utf8",
    );

    expect(composer).toContain("showExtensions");
    expect(composer).toContain("v-if=\"showExtensions\"");
    expect(composer).toContain(":editor-label=\"composerLabel ?? `${channelName} composer`\"");
    expect(editor).toContain("editorLabel");
    expect(editor).toContain("\"aria-label\": props.editorLabel");
  });

  test("call-chat sends use the thread handler while the main composer keeps the channel handler", () => {
    const contentArea = readFileSync(
      new URL("../src/components/chat/ContentArea.vue", import.meta.url),
      "utf8",
    );
    const readyShell = readFileSync(
      new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url),
      "utf8",
    );

    expect(contentArea).toContain("@send=\"onSend\"");
    expect(contentArea).toContain("@send-call-chat=\"(...args) => emit('sendCallChat', ...args)\"");
    expect(readyShell).toContain("@send=\"sendActiveMessage\"");
    expect(readyShell).toContain("@send-call-chat=\"sendThreadMessage\"");
  });
});

function message(overrides: Partial<TimelineMessage>): TimelineMessage {
  return {
    id: "message",
    body: "",
    author: "alice",
    createdAt: "2026-06-09T12:00:00Z",
    timestamp: Date.parse("2026-06-09T12:00:00Z"),
    isSelf: false,
    ...overrides,
  };
}
