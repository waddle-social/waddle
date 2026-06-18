import { afterEach, describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import type { RemoteMediaTrack } from "../src/lib/calls/engine";
import { $callSelfViewHidden, resetCallViewState } from "../src/lib/calls/call-view-state";

function gridSource(): string {
  return readFileSync(
    new URL("../src/components/calls/CallTileGrid.vue", import.meta.url),
    "utf8",
  );
}

function remoteCamera(identity: string): RemoteMediaTrack {
  return {
    participantIdentity: identity,
    publicationSid: `${identity}-cam`,
    kind: "video",
    source: "camera",
    track: {} as RemoteMediaTrack["track"],
  };
}

describe("CallTileGrid overflow tile", () => {
  test("derives the visible tiles from partitionStageTiles, not a raw clip", () => {
    const source = gridSource();
    expect(source).toContain("partitionStageTiles");
    // The old silent clip must be gone.
    expect(source).not.toContain("tiles.value.slice(0, layout.value.maxTiles)");
  });

  test("renders a '+N more' overflow cell bound to the hidden count", () => {
    const source = gridSource();
    expect(source).toContain("call-tile-grid__overflow");
    // The cell is an activatable button labelled for assistive tech.
    expect(source).toContain("aria-label");
    expect(source).toMatch(/hiddenCount/);
    expect(source).toContain("more");
  });

  test("emits open-participants when the overflow cell is activated", () => {
    const source = gridSource();
    expect(source).toContain("openParticipants");
    expect(source).toContain('emit("openParticipants")');
  });

  test("a degenerate 1-cell Stage renders one tile and no overflow cell", async () => {
    // SSR has no ResizeObserver, so the container measures 0×0 and the layout
    // collapses to 1×1. With several participants the grid must show the single
    // tile and never an overflow cell (matching the partition's degenerate
    // rule) — nobody is silently promoted into a phantom overflow.
    const html = await renderVueComponent(
      "../src/components/calls/CallTileGrid.vue",
      {
        remoteTracks: [
          remoteCamera("a@waddle.test/web"),
          remoteCamera("b@waddle.test/web"),
          remoteCamera("c@waddle.test/web"),
        ],
        localTracks: [],
        localIdentity: "me@waddle.test/web",
        micEnabled: true,
      },
      import.meta.url,
    );
    expect(html).not.toContain("call-tile-grid__overflow");
  });
});

describe("CallTileGrid hide self-view (#1021)", () => {
  afterEach(() => {
    resetCallViewState();
  });

  test("keeps the local self-view tile on stage by default", async () => {
    const html = await renderVueComponent(
      "../src/components/calls/CallTileGrid.vue",
      {
        remoteTracks: [remoteCamera("bob@waddle.test/web")],
        localTracks: [],
        localIdentity: "me@waddle.test/web",
        micEnabled: true,
      },
      import.meta.url,
    );
    // The degenerate SSR 1×1 grid shows the first tile, which is the self tile.
    expect(html).toContain("You");
  });

  test("drops the local self-view tile from the stage when self-view is hidden", async () => {
    $callSelfViewHidden.set(true);
    const html = await renderVueComponent(
      "../src/components/calls/CallTileGrid.vue",
      {
        remoteTracks: [remoteCamera("bob@waddle.test/web")],
        localTracks: [],
        localIdentity: "me@waddle.test/web",
        micEnabled: true,
      },
      import.meta.url,
    );
    expect(html).not.toContain("You");
    expect(html).toContain("bob");
  });
});

describe("surfaces wire the hide self-view control (#1021)", () => {
  function read(rel: string): string {
    return readFileSync(new URL(rel, import.meta.url), "utf8");
  }

  for (const surface of ["CallSplitContainer", "CallExpandedSurface"] as const) {
    test(`${surface} binds the self-view state and toggle to CallControls`, () => {
      const source = read(`../src/components/calls/${surface}.vue`);
      expect(source).toContain("$callSelfViewHidden");
      expect(source).toContain("toggleCallSelfViewHidden");
      expect(source).toContain(":self-view-hidden=");
      expect(source).toContain('@toggle-self-view="toggleCallSelfViewHidden"');
    });
  }
});

describe("surfaces wire the overflow tile to the Participants panel", () => {
  function read(rel: string): string {
    return readFileSync(new URL(rel, import.meta.url), "utf8");
  }

  test("Expanded surface opens the dock in place on overflow", () => {
    const source = read("../src/components/calls/CallExpandedSurface.vue");
    expect(source).toContain("openCallParticipants");
    expect(source).toContain('@open-participants="openCallParticipants"');
  });

  test("Split surface bumps to Expanded with the Participants dock on overflow", () => {
    const source = read("../src/components/calls/CallSplitContainer.vue");
    // The Split surface has no room for the dock, so the overflow tile reuses
    // the same bump-to-Expanded path as its Participants button.
    expect(source).toContain('@open-participants="enterExpandedWithDock"');
  });
});
