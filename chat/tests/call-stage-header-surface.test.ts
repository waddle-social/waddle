import { afterEach, describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import { $callState, clearCallState } from "../src/lib/calls/call-store";
import { $callUiMode } from "../src/lib/calls/ui-mode";
import { resetCallActiveSince } from "../src/lib/calls/call-duration";
import type { LiveKitJoin } from "../src/lib/calls/types";

const join: LiveKitJoin = {
  url: "wss://livekit.test",
  room: "bob@waddle.test::c1",
  identity: "me@waddle.test/web",
  token: "jwt",
};

afterEach(() => {
  clearCallState();
  $callUiMode.set("split");
  resetCallActiveSince();
});

function setActiveDmCall(): void {
  $callState.set({
    phase: "active",
    peer: "bob@waddle.test/web",
    sid: "c1",
    media: { audio: true, video: false },
    join,
    kind: "dm",
  });
  $callUiMode.set("split");
}

describe("CallSplitContainer stage-header integration", () => {
  test("mounts the stage-header with the call title and the relocated connection indicator", async () => {
    setActiveDmCall();
    const html = await renderVueComponent(
      "../src/components/calls/CallSplitContainer.vue",
      { dmPeerJid: "bob@waddle.test", dmPeerName: "Bob" },
      import.meta.url,
    );
    // Title in the new status-only header.
    expect(html).toContain("Bob");
    // Connection indicator now lives in the header (rendered on the surface).
    expect(html).toContain("call-connection");
    // The timer reads "Connecting…" until the engine stamps the call clock.
    expect(html).toContain("Connecting…");
  });
});
