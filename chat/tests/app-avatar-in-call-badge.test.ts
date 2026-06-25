import { describe, expect, test } from "bun:test";

import { renderVueComponent } from "./helpers/render-vue-sfc";

function render(props: Record<string, unknown>) {
  return renderVueComponent("../src/components/ui/AppAvatar.vue", props, import.meta.url);
}

describe("AppAvatar in-call badge", () => {
  test("renders an accessible in-call badge when inCall is true", async () => {
    const html = await render({ name: "Alice", presence: "online", inCall: true });
    expect(html).toContain("app-avatar-call-badge");
    expect(html).toContain("In a call");
  });

  test("renders no in-call badge by default", async () => {
    const html = await render({ name: "Alice", presence: "online" });
    expect(html).not.toContain("app-avatar-call-badge");
  });

  test("the badge layers on top of the presence dot, not replacing it", async () => {
    const html = await render({ name: "Alice", presence: "online", inCall: true });
    // Overlay, never replacement (ADR-010): the Show dot must still render.
    expect(html).toContain("app-avatar-presence-dot");
    expect(html).toContain("app-avatar-call-badge");
  });
});
