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

function dockSource(): string {
  return readFileSync(
    new URL("../src/components/calls/CallDock.vue", import.meta.url),
    "utf8",
  );
}

describe("CallDock", () => {
  test("renders Participants and Chat tabs with the active one selected and the roster embedded", async () => {
    const html = await renderVueComponent(
      "../src/components/calls/CallDock.vue",
      { rows: rosterFixture(), activeTab: "participants", chatUnread: 0 },
      import.meta.url,
    );

    expect(html).toContain('role="tablist"');
    // Both tabs and both panels exist and are associated for screen readers.
    expect(html).toContain('id="call-dock-tab-participants"');
    expect(html).toContain('id="call-dock-tab-chat"');
    expect(html).toContain('aria-labelledby="call-dock-tab-participants"');
    expect(html).toContain('aria-labelledby="call-dock-tab-chat"');
    expect(html).toContain("Participants");
    expect(html).toContain("Chat");
    // The Participants tab is the selected one; Chat is not.
    expect(html).toMatch(/id="call-dock-tab-participants"[^>]*aria-selected="true"/);
    expect(html).toMatch(/id="call-dock-tab-chat"[^>]*aria-selected="false"/);
    // The roster panel is embedded for real (not stubbed).
    expect(html).toContain("You");
    expect(html).toContain("alice");
    expect(html).toContain('aria-label="Close dock"');
  });

  test("selects the Chat tab when it is the active tab", async () => {
    const html = await renderVueComponent(
      "../src/components/calls/CallDock.vue",
      { rows: rosterFixture(), activeTab: "chat", chatUnread: 0 },
      import.meta.url,
    );

    expect(html).toMatch(/id="call-dock-tab-chat"[^>]*aria-selected="true"/);
    expect(html).toMatch(/id="call-dock-tab-participants"[^>]*aria-selected="false"/);
  });

  test("shows an unread badge on the Chat tab only when there are unread messages", async () => {
    const withUnread = await renderVueComponent(
      "../src/components/calls/CallDock.vue",
      { rows: rosterFixture(), activeTab: "participants", chatUnread: 5 },
      import.meta.url,
    );
    expect(withUnread).toContain('aria-label="5 unread messages"');
    expect(withUnread).toContain(">5<");

    const noUnread = await renderVueComponent(
      "../src/components/calls/CallDock.vue",
      { rows: rosterFixture(), activeTab: "participants", chatUnread: 0 },
      import.meta.url,
    );
    expect(noUnread).not.toContain("unread messages");
  });

  test("forwards roster events, emits tab selection + close, and projects chat via a slot", () => {
    const source = dockSource();
    expect(source).toContain("CallParticipantsPanel");
    expect(source).toContain("@set-volume");
    expect(source).toContain("@reset-all");
    expect(source).toContain("emit('close')");
    expect(source).toContain("emit('setTab'");
    expect(source).toContain('name="chat"');
  });
});
