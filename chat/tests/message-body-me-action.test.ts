import { describe, expect, test } from "bun:test";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { renderVueComponent } from "./helpers/render-vue-sfc";

function message(overrides: Partial<TimelineMessage>): TimelineMessage {
  return {
    id: "m1",
    author: "Atlas",
    body: "",
    createdAt: "2026-06-02T12:00:00.000Z",
    createdAtSource: "fallback",
    isSelf: false,
    ...overrides,
  };
}

function renderMessageBody(msg: TimelineMessage, compact = false): Promise<string> {
  return renderVueComponent("../src/components/chat/MessageBody.vue", { message: msg, compact }, import.meta.url);
}

describe("MessageBody XEP-0245 /me rendering", () => {
  test("renders `/me` bodies as an italic `* Author action` line", async () => {
    const html = await renderMessageBody(message({ body: "/me shrugs in disgust" }));
    expect(html).toMatch(/class="[^"]*styled-body[^"]*italic/);
    expect(html).toContain('<span class="font-semibold">* Atlas</span> shrugs in disgust');
    expect(html).not.toContain("/me ");
  });

  test("keeps markup aligned on the action text", async () => {
    const html = await renderMessageBody(message({
      body: "/me really waves",
      markup: [{ type: "span", start: 4, end: 10, styles: ["strong"] }],
    }));
    expect(html).toContain("* Atlas</span> <strong>really</strong> waves");
  });

  test("an edited (XEP-0308) `/me` body renders the same way", async () => {
    const html = await renderMessageBody(message({ body: "/me waves again", isEdited: true }), true);
    expect(html).toContain("* Atlas</span> waves again");
  });

  test("non-matching bodies render normally", async () => {
    const html = await renderMessageBody(message({ body: "/meshrugs" }));
    expect(html).toContain("<p>/meshrugs</p>");
    expect(html).not.toMatch(/styled-body[^"]*italic/);
  });
});
