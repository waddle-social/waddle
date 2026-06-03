import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";

describe("UnreadView refresh wiring", () => {
  test("refresh button hydrates inbox state before rebuilding unread overview", () => {
    const source = readFileSync(new URL("../src/components/chat/UnreadView.vue", import.meta.url), "utf8");
    const handlerStart = source.indexOf("async function refreshUnread()");
    expect(handlerStart).toBeGreaterThanOrEqual(0);
    const handler = source.slice(handlerStart, source.indexOf("</script>"));

    expect(source).toContain("onRefreshInbox?: () => void | Promise<unknown>;");
    expect(handler).toContain("await props.onRefreshInbox?.();");
    expect(handler.indexOf("await props.onRefreshInbox?.();")).toBeLessThan(handler.indexOf("await refresh();"));
    expect(source).toContain("@click=\"refreshUnread()\"");
  });
});
