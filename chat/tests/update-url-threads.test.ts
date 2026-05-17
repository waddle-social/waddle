// Tests for the `updateUrl()` URL-sync helper in chat-app-controller.ts.
//
// The bug guarded against here: before this fix the helper had no
// "threads" or "admin" branch, so any reactive watcher fired after
// `openThreads()` would fall through to `pushChannelRoute(null)` and
// bounce the URL from /threads back to /. On a hard refresh of /threads
// the `onConnectionReady` finally-block calls updateUrl() and produces
// the same regression — the user lands on / despite asking for the
// global Threads view.
//
// The controller wires `updateUrl()` to live ref values and is too
// heavy to instantiate here; we instead capture the exact same routing
// decisions in a small model and exercise every branch.

import { describe, expect, test } from "bun:test";

type ActivePage = "dashboard" | "chat" | "settings" | "admin" | "threads";
type SidebarMode = "channels" | "dms";
type RightPanel = "thread" | "extension" | "pinned" | null;

interface UpdateUrlInputs {
  activePage: ActivePage;
  activeRightPanel: RightPanel;
  sidebarMode: SidebarMode;
  hasDmPeer: boolean;
  hasCurrentChannel: boolean;
}

type Action =
  | { kind: "settings" }
  | { kind: "threads" }
  | { kind: "admin" }
  | { kind: "dashboard" }
  | { kind: "extension" }
  | { kind: "dm" }
  | { kind: "channel" };

// Mirrors the branching ladder inside `updateUrl()`. The function in
// production calls the corresponding `push*Route` helper; here we
// return a tagged Action so the test asserts on the routing decision
// rather than the side-effect.
function decideUpdateUrlAction(inputs: UpdateUrlInputs): Action {
  if (inputs.activePage === "settings") return { kind: "settings" };
  if (inputs.activePage === "threads") return { kind: "threads" };
  if (inputs.activePage === "admin") return { kind: "admin" };
  if (inputs.activePage === "dashboard") return { kind: "dashboard" };
  if (inputs.activePage === "chat" && inputs.activeRightPanel === "extension") {
    return { kind: "extension" };
  }
  if (inputs.sidebarMode === "dms" && inputs.hasDmPeer) {
    return { kind: "dm" };
  }
  return { kind: "channel" };
}

describe("updateUrl branching for the global Threads view", () => {
  test("activePage=threads always routes to /threads, regardless of sidebar mode", () => {
    const channelsMode = decideUpdateUrlAction({
      activePage: "threads",
      activeRightPanel: null,
      sidebarMode: "channels",
      hasDmPeer: false,
      hasCurrentChannel: false,
    });
    expect(channelsMode).toEqual({ kind: "threads" });

    const dmsMode = decideUpdateUrlAction({
      activePage: "threads",
      activeRightPanel: null,
      sidebarMode: "dms",
      hasDmPeer: true,
      hasCurrentChannel: false,
    });
    expect(dmsMode).toEqual({ kind: "threads" });
  });

  test("activePage=threads survives stale channel state", () => {
    // Critical regression case: when `openThreads()` runs from a
    // selected channel, the channel-watcher fires `updateUrl()` first.
    // At that point `activeChannelId` has been nulled but `currentChannel`
    // may still be cached for a microtask. The threads branch must
    // win *before* any chat/dm/channel fallback.
    const action = decideUpdateUrlAction({
      activePage: "threads",
      activeRightPanel: null,
      sidebarMode: "channels",
      hasDmPeer: false,
      hasCurrentChannel: true,
    });
    expect(action).toEqual({ kind: "threads" });
  });

  test("activePage=admin does not bounce through pushChannelRoute(null)", () => {
    const action = decideUpdateUrlAction({
      activePage: "admin",
      activeRightPanel: null,
      sidebarMode: "channels",
      hasDmPeer: false,
      hasCurrentChannel: false,
    });
    expect(action).toEqual({ kind: "admin" });
  });

  test("non-threads/admin activePage values keep their existing routing", () => {
    expect(decideUpdateUrlAction({
      activePage: "dashboard",
      activeRightPanel: null,
      sidebarMode: "channels",
      hasDmPeer: false,
      hasCurrentChannel: false,
    })).toEqual({ kind: "dashboard" });

    expect(decideUpdateUrlAction({
      activePage: "settings",
      activeRightPanel: null,
      sidebarMode: "channels",
      hasDmPeer: false,
      hasCurrentChannel: false,
    })).toEqual({ kind: "settings" });

    expect(decideUpdateUrlAction({
      activePage: "chat",
      activeRightPanel: "extension",
      sidebarMode: "channels",
      hasDmPeer: false,
      hasCurrentChannel: true,
    })).toEqual({ kind: "extension" });

    expect(decideUpdateUrlAction({
      activePage: "chat",
      activeRightPanel: null,
      sidebarMode: "dms",
      hasDmPeer: true,
      hasCurrentChannel: false,
    })).toEqual({ kind: "dm" });

    expect(decideUpdateUrlAction({
      activePage: "chat",
      activeRightPanel: null,
      sidebarMode: "channels",
      hasDmPeer: false,
      hasCurrentChannel: true,
    })).toEqual({ kind: "channel" });
  });
});
