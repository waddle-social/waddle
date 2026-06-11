import { afterEach, describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import { $callState, clearCallState } from "../src/lib/calls/call-store";

describe("OutgoingCallToast", () => {
  afterEach(() => {
    clearCallState();
  });

  test("renders calling before the peer reports a ringing device", async () => {
    $callState.set({
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "c1",
      media: { audio: true, video: false },
      initiator: "alice@waddle.test/web",
    });

    const html = await render();
    expect(html).toContain("Audio call");
    expect(html).toContain("calling");
    expect(html).not.toContain("ringing");
  });

  test("renders ringing after XEP-0353 ringing arrives", async () => {
    $callState.set({
      phase: "outgoing",
      to: "bob@waddle.test",
      sid: "c1",
      media: { audio: true, video: true },
      initiator: "alice@waddle.test/web",
      ringing: true,
    });

    const html = await render();
    expect(html).toContain("Video call");
    expect(html).toContain("ringing");
    expect(html).not.toContain("calling");
  });
});

function render(): Promise<string> {
  return renderVueComponent("../src/components/calls/OutgoingCallToast.vue", {}, import.meta.url);
}
