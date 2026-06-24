import { afterEach, describe, expect, test } from "bun:test";

import { renderVueComponent } from "./helpers/render-vue-sfc";
import { $presenceMode } from "../src/presence/presence-store";

function render() {
  return renderVueComponent("../src/components/chat/PresencePicker.vue", {}, import.meta.url);
}

describe("PresencePicker", () => {
  afterEach(() => {
    $presenceMode.set({ kind: "automatic" });
  });

  test("renders the three pickable statuses as a radiogroup", async () => {
    const html = await render();
    expect(html).toContain('role="radiogroup"');
    expect(html).toContain("Available");
    expect(html).toContain("Away");
    expect(html).toContain("Do Not Disturb");
  });

  test("shows the Automatic hint by default and no reset action", async () => {
    const html = await render();
    expect(html).toContain("Automatic");
    expect(html).not.toContain("Reset to automatic");
  });

  test("offers Reset to automatic once a manual status is set", async () => {
    $presenceMode.set({ kind: "manual", status: "dnd" });
    const html = await render();
    expect(html).toContain("Reset to automatic");
    expect(html).toContain('aria-checked="true"');
  });
});
