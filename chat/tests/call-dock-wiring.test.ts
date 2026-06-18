import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderVueComponent } from "./helpers/render-vue-sfc";

function surfaceSource(name: string): string {
  return readFileSync(
    new URL(`../src/components/calls/${name}`, import.meta.url),
    "utf8",
  );
}

describe("CallControls participants toggle", () => {
  test("renders a Participants toggle with the attendee count instead of a volume button", async () => {
    const html = await renderVueComponent(
      "../src/components/calls/CallControls.vue",
      {
        micEnabled: true,
        camEnabled: true,
        screenShareEnabled: false,
        screenShareSupported: false,
        isExpanded: true,
        participantsOpen: false,
        participantCount: 3,
        viewMode: "gallery",
      },
      import.meta.url,
    );

    expect(html).toContain('aria-label="Open participants"');
    expect(html).toContain(">3<");
    expect(html).not.toContain("volume mixer");
  });

  test("drops the old volume button and wires the participants emit", () => {
    const source = surfaceSource("CallControls.vue");
    expect(source).toContain("toggleParticipants");
    expect(source).toContain("participantsOpen");
    expect(source).toContain("participantCount");
    expect(source).not.toContain("toggleVolume");
    expect(source).not.toContain("volumeOpen");
  });
});

describe("call surfaces use the Participants dock", () => {
  test("both surfaces drive the roster + dock and drop the volume mixer dialog", () => {
    for (const name of ["CallSplitContainer.vue", "CallExpandedSurface.vue"]) {
      const source = surfaceSource(name);
      expect(source).toContain("useCallRoster");
      expect(source).not.toContain("CallVolumeMixerDialog");
      expect(source).not.toContain("useCallVolumeMixer");
    }
  });

  test("Split's Participants button bumps to Expanded with the dock open", () => {
    const source = surfaceSource("CallSplitContainer.vue");
    expect(source).toContain("openCallDock");
    expect(source).toContain('$callUiMode.set("expanded")');
    expect(source).toContain("@toggle-participants");
  });

  test("Expanded toggles the dock, renders it gated on open, and reflows the stage", () => {
    const source = surfaceSource("CallExpandedSurface.vue");
    expect(source).toContain("$callDockOpen");
    expect(source).toContain("@toggle-participants=\"toggleCallDock\"");
    // The dock renders only when open, as a sibling of the grid inside
    // __main (a flex row), so the stage reflows beside it.
    expect(source).toContain("<CallDock");
    expect(source).toContain("v-if=\"dockOpen\"");
  });

  test("Expanded Escape closes the open dock before collapsing the call", () => {
    const source = surfaceSource("CallExpandedSurface.vue");
    const start = source.indexOf("function onKeydown");
    const guard = source.slice(start, source.indexOf("collapseToSplit()", start));
    // The guard must actually close the dock (not merely mention the flag)
    // and short-circuit before the collapse.
    expect(guard).toContain("if (dockOpen.value)");
    expect(guard).toContain("closeCallDock()");
  });
});
