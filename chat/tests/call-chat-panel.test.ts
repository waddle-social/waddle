import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import type { TimelineMessage } from "../src/lib/chat-ui";

function message(id: string, body: string, author = "alice"): TimelineMessage {
  return { id, author, body, createdAt: "2026-06-18T12:00:00Z", isSelf: false };
}

function panelSource(): string {
  return readFileSync(
    new URL("../src/components/calls/CallChatPanel.vue", import.meta.url),
    "utf8",
  );
}

describe("CallChatPanel", () => {
  test("renders the call-thread messages and the call-chat composer", async () => {
    const html = await renderVueComponent(
      "../src/components/calls/CallChatPanel.vue",
      {
        messages: [message("m1", "hello call"), message("m2", "second")],
        draft: "",
        currentUser: "me",
        avatarUrlByAuthor: {},
      },
      import.meta.url,
    );

    expect(html).toContain("hello call");
    expect(html).toContain("second");
    expect(html).toContain("alice");
    // The moved composer is present.
    expect(html).toContain("Call chat composer");
  });

  test("shows an empty state when the call thread has no messages yet", async () => {
    const html = await renderVueComponent(
      "../src/components/calls/CallChatPanel.vue",
      { messages: [], draft: "", currentUser: "me", avatarUrlByAuthor: {} },
      import.meta.url,
    );

    expect(html).toContain("No messages yet");
  });

  test("renders message bodies via MessageBody and forwards the composer send", () => {
    const source = panelSource();
    expect(source).toContain("MessageBody");
    expect(source).toContain("MessageComposer");
    expect(source).toContain("emit('send'");
    // The in-call composer keeps extensions hidden, matching the old footer.
    expect(source).toContain(":show-extensions=\"false\"");
  });
});
