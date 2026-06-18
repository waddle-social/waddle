import { afterEach, describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import {
  resetCallActiveSince,
  setCallActiveSince,
} from "../src/lib/calls/call-duration";

afterEach(() => {
  resetCallActiveSince();
});

async function renderHeader(props: Record<string, unknown>): Promise<string> {
  return renderVueComponent(
    "../src/components/calls/CallStageHeader.vue",
    props,
    import.meta.url,
  );
}

describe("CallStageHeader", () => {
  test("shows the call title", async () => {
    const html = await renderHeader({ title: "Alice Cooper", subline: "Audio call" });
    expect(html).toContain("Alice Cooper");
  });

  test("shows the contextual subline next to the title", async () => {
    const html = await renderHeader({
      title: "Design sync",
      subline: "2 others in call",
    });
    expect(html).toContain("2 others in call");
  });

  test("shows 'Connecting…' instead of a timer before the call clock starts", async () => {
    // $callActiveSince left null (reset in afterEach) → not yet connected.
    const html = await renderHeader({ title: "Alice", subline: "Audio call" });
    expect(html).toContain("Connecting…");
    expect(html).not.toContain("0:00");
  });

  test("shows the live elapsed timer once the call clock has started", async () => {
    const fixed = 1_700_000_000_000;
    const original = Date.now;
    Date.now = () => fixed;
    try {
      setCallActiveSince(fixed - 65_000); // 1m 05s ago
      const html = await renderHeader({ title: "Alice", subline: "Audio call" });
      expect(html).toContain("1:05");
      expect(html).not.toContain("Connecting…");
    } finally {
      Date.now = original;
    }
  });

  test("drops role=timer (and goes live) only once the call clock is running", async () => {
    // Connecting: no real duration yet, so no timer role and the dot is muted.
    const connecting = await renderHeader({ title: "Alice", subline: "Audio call" });
    expect(connecting).not.toContain('role="timer"');
    expect(connecting).not.toContain("call-stage-header__live-dot--live");

    const fixed = 1_700_000_000_000;
    const original = Date.now;
    Date.now = () => fixed;
    try {
      setCallActiveSince(fixed - 5_000);
      const running = await renderHeader({ title: "Alice", subline: "Audio call" });
      expect(running).toContain('role="timer"');
      expect(running).toContain("call-stage-header__live-dot--live");
    } finally {
      Date.now = original;
    }
  });

  test("hosts the connection-quality indicator (relocated from the control bar)", async () => {
    const html = await renderHeader({ title: "Alice", subline: "Audio call" });
    expect(html).toContain("call-connection");
  });

  test("keeps the recording indicator inert by default", async () => {
    const html = await renderHeader({ title: "Alice", subline: "Audio call" });
    // The live-region slot exists (reserved for the future recording slice)…
    expect(html).toContain('class="call-stage-header__recording"');
    // …but shows no dot and no label until recording is wired on.
    expect(html).not.toContain("call-stage-header__recording-dot");
    expect(html).not.toContain(">Recording<");
  });

  test("renders the recording indicator when wired on (future recording slice)", async () => {
    const html = await renderHeader({
      title: "Alice",
      subline: "Audio call",
      recording: true,
    });
    expect(html).toContain("call-stage-header__recording-dot");
    expect(html).toContain(">Recording<");
  });
});
