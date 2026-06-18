import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import { buildCallRoster } from "../src/lib/calls/call-roster";

function rosterFixture() {
  return buildCallRoster({
    remoteParticipantIdentities: ["alice@waddle.test/web"],
    remoteTracks: [],
    localIdentity: "me@waddle.test/browser",
    localMicEnabled: true,
    localCameraEnabled: true,
    activeSpeakerIdentities: new Set<string>(),
    volumeRows: [],
  });
}

describe("CallDock", () => {
  test("renders a Participants tab, the embedded roster, and an accessible close control", async () => {
    const html = await renderVueComponent(
      "../src/components/calls/CallDock.vue",
      { rows: rosterFixture() },
      import.meta.url,
    );

    expect(html).toContain('role="tablist"');
    expect(html).toContain('role="tab"');
    expect(html).toContain('aria-selected="true"');
    // The tab and its panel are associated both ways for screen readers.
    expect(html).toContain('id="call-dock-tab-participants"');
    expect(html).toContain('role="tabpanel"');
    expect(html).toContain('aria-labelledby="call-dock-tab-participants"');
    expect(html).toContain("Participants");
    // The roster panel is embedded for real (not stubbed).
    expect(html).toContain("You");
    expect(html).toContain("alice");
    expect(html).toContain('aria-label="Close participants"');
  });

  test("forwards the panel's volume events and exposes a close emit", () => {
    const source = readFileSync(
      new URL("../src/components/calls/CallDock.vue", import.meta.url),
      "utf8",
    );
    expect(source).toContain("CallParticipantsPanel");
    expect(source).toContain("@set-volume");
    expect(source).toContain("@reset-all");
    expect(source).toContain("emit('close')");
  });
});
