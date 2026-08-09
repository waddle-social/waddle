import { describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";

// Codex 1668 round: dashboard / feed / events / threads / unread / settings
// are sibling branches of ContentArea, so they render no connection banner.
// The sticky superseded latch rejects every automatic reconnect, which left a
// superseded tab parked on those surfaces with no visible recovery action.
// ChatReadyShell now renders this banner for exactly those branches.
describe("SupersededRecoveryBanner", () => {
  test("renders the superseded copy and the Reconnect action", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/SupersededRecoveryBanner.vue",
      { detail: "This session was resumed in another tab." },
      import.meta.url,
    );

    expect(html).toContain("Session resumed in another tab");
    expect(html).toContain("This session was resumed in another tab.");
    expect(html).toContain("Reconnect to continue from this tab.");
    expect(html).toContain("Reconnect");
    expect(html).toContain('role="status"');
  });

  test("falls back to the default detail copy when none is provided", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/SupersededRecoveryBanner.vue",
      { detail: "   " },
      import.meta.url,
    );

    expect(html).toContain("This session was resumed in another tab.");
  });
});
