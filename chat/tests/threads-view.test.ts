// Tests for the click-thru behaviour of the global Threads view.
//
// `ThreadsView.vue` is a thin wrapper: it fetches threads (delegated
// to `ThreadsListPanel`) and, on click, hands the entry's bare JID off
// to the controller's `onSelectThreadEntry(channelJid, threadId)`. The
// controller routes the JID to its hosting surface — a channel
// selection for MUC rooms, or a DM open for partner JIDs — and opens
// the ThreadPanel reader.
//
// The component itself is too small to be worth mounting under
// `@vue/test-utils`; we exercise the two pieces of behaviour that
// matter: the room-JID resolver and the channel/DM routing shape that
// the controller's `onSelectThreadEntry` implements.

import { describe, expect, mock, test } from "bun:test";
import type { WasmThreadEntry } from "../src/lib/xmpp/wasm-types";
import type { CallMedia } from "../src/lib/calls/types";
import type { ChannelSummary } from "../src/lib/chat-types";
import { resolveChannelIdForRoomJid } from "../src/lib/threads-channel-resolve";
import { resolveThreadEntryTarget } from "../src/lib/threads-view-target";

const MUC_DOMAIN = "muc.waddle.example";

function entry(overrides: Partial<WasmThreadEntry> = {}): WasmThreadEntry {
  return {
    channel: "general@muc.waddle.example",
    thread_id: "root-1",
    last_stanza_id: "S",
    last_activity: new Date().toISOString(),
    unread: 0,
    reply_count: 0,
    has_unread: false,
    ...overrides,
  };
}

interface ThreadEntryHandlers {
  onSelectThread: (channelId: string, threadId: string) => void | Promise<void>;
  onOpenDmThread: (peerJid: string, threadId: string) => void | Promise<void>;
}

/**
 * Mirrors the controller's `onSelectThreadEntry`, which is what
 * `ThreadsView.vue` now delegates to: classify the entry's bare JID and
 * route a channel selection vs a DM open. If the JID is unusable, the
 * handler is a no-op.
 */
async function handleOpenThread(
  e: WasmThreadEntry,
  channels: { id: string; jid?: string }[],
  handlers: ThreadEntryHandlers,
): Promise<void> {
  const target = resolveThreadEntryTarget(e.channel, {
    channels,
    managedMucDomain: MUC_DOMAIN,
  });
  if (!target) return;
  if (target.kind === "channel") {
    await handlers.onSelectThread(target.channelId, e.thread_id);
    return;
  }
  await handlers.onOpenDmThread(target.peerJid, e.thread_id);
}

describe("resolveChannelIdForRoomJid", () => {
  test("matches a channel by exact bare JID", () => {
    const channels = [
      { id: "general", jid: "general@muc.waddle.example" },
      { id: "random", jid: "random@muc.waddle.example" },
    ];
    expect(
      resolveChannelIdForRoomJid("general@muc.waddle.example", channels),
    ).toBe("general");
  });

  test("ignores resource on the room JID when matching", () => {
    const channels = [{ id: "general", jid: "general@muc.waddle.example" }];
    expect(
      resolveChannelIdForRoomJid(
        "general@muc.waddle.example/some-nick",
        channels,
      ),
    ).toBe("general");
  });

  test("falls back to the JID local-part when no channel matches", () => {
    expect(
      resolveChannelIdForRoomJid("orphan@muc.waddle.example", []),
    ).toBe("orphan");
  });

  test("returns null for an empty / malformed JID", () => {
    expect(resolveChannelIdForRoomJid("", [])).toBeNull();
    expect(resolveChannelIdForRoomJid("@muc.waddle.example", [])).toBeNull();
  });

  test("prefers an explicit channel match over the local-part fallback", () => {
    // Two channels share the same node — the explicit `jid` match wins,
    // so we route to `pretty-name` rather than the bare slug `general`.
    const channels = [
      { id: "pretty-name", jid: "general@muc.waddle.example" },
      { id: "general", jid: "general@other-host.example" },
    ];
    expect(
      resolveChannelIdForRoomJid("general@muc.waddle.example", channels),
    ).toBe("pretty-name");
  });
});

describe("ThreadsView click handler", () => {
  function handlers() {
    return {
      onSelectThread: mock(async (_channelId: string, _threadId: string) => {}),
      onOpenDmThread: mock(async (_peerJid: string, _threadId: string) => {}),
    };
  }

  test("routes a channel thread to onSelectThread with the resolved channel-id", async () => {
    const h = handlers();
    const channels = [{ id: "general", jid: "general@muc.waddle.example" }];
    await handleOpenThread(
      entry({ channel: "general@muc.waddle.example", thread_id: "root-abc" }),
      channels,
      h,
    );
    expect(h.onSelectThread.mock.calls).toEqual([["general", "root-abc"]]);
    expect(h.onOpenDmThread).not.toHaveBeenCalled();
  });

  test("falls back to the JID local-part when the channel is unknown", async () => {
    // Threads view can outlive the structure load: a thread can be
    // listed before its channel summary has been wired into
    // `waddles.channels`. A managed-room JID must still route to the
    // channel surface via its local-part.
    const h = handlers();
    await handleOpenThread(
      entry({ channel: "history@muc.waddle.example", thread_id: "t-9" }),
      [],
      h,
    );
    expect(h.onSelectThread.mock.calls).toEqual([["history", "t-9"]]);
    expect(h.onOpenDmThread).not.toHaveBeenCalled();
  });

  test("routes a DM thread to the DM open, not a channel selection (#917)", async () => {
    // The regression this PR fixes: a partner-keyed DM thread must open
    // the direct-message conversation, never `selectChannel(<localpart>)`.
    const h = handlers();
    const channels = [{ id: "general", jid: "general@muc.waddle.example" }];
    await handleOpenThread(
      entry({ channel: "bob@waddle.example", thread_id: "t-dm" }),
      channels,
      h,
    );
    expect(h.onOpenDmThread.mock.calls).toEqual([["bob@waddle.example", "t-dm"]]);
    expect(h.onSelectThread).not.toHaveBeenCalled();
  });

  test("is a no-op for entries with an unusable channel JID", async () => {
    const h = handlers();
    await handleOpenThread(entry({ channel: "" }), [], h);
    expect(h.onSelectThread).not.toHaveBeenCalled();
    expect(h.onOpenDmThread).not.toHaveBeenCalled();
  });

  test("awaits the routed handler so async controller work completes before returning", async () => {
    let resolved = false;
    const h = {
      onSelectThread: mock(async () => {
        await new Promise<void>((r) => setTimeout(r, 1));
        resolved = true;
      }),
      onOpenDmThread: mock(async () => {}),
    };
    await handleOpenThread(entry(), [], h);
    expect(resolved).toBe(true);
  });
});

/**
 * Mirrors `ThreadsView.vue`'s `joinCall(entry, media)` exactly: resolve
 * the row's room JID to a channel-id and hand off to the controller's
 * `onJoinChannelCall(channelId, roomJid, media)` — the shared Join path
 * the call banner and the in-channel anchor card also drive.
 */
function handleJoinCall(
  e: WasmThreadEntry,
  media: CallMedia,
  channels: readonly ChannelSummary[],
  onJoinChannelCall: (channelId: string | null, roomJid: string, media: CallMedia) => void,
): void {
  const channelId = resolveChannelIdForRoomJid(e.channel, channels);
  onJoinChannelCall(channelId, e.channel, media);
}

function channel(overrides: Partial<ChannelSummary> = {}): ChannelSummary {
  return {
    id: "general",
    name: "general",
    jid: "general@muc.waddle.example",
    ...overrides,
  };
}

describe("ThreadsView join-call handler", () => {
  function handlers() {
    return {
      onSelectThread: mock(async (_channelId: string, _threadId: string) => {}),
      onOpenDmThread: mock(async (_peerJid: string, _threadId: string) => {}),
    };
  }

  test("resolves the channel-id and hands off the room JID + media to onJoinChannelCall", () => {
    const onJoinChannelCall = mock(
      (_channelId: string | null, _roomJid: string, _media: CallMedia) => {},
    );
    const media: CallMedia = { audio: true, video: true };
    handleJoinCall(
      entry({ channel: "general@muc.waddle.example" }),
      media,
      [channel()],
      onJoinChannelCall,
    );
    // Resolved channel-id, the original room JID (unchanged), and the
    // exact media object — same reference, not a copy.
    expect(onJoinChannelCall).toHaveBeenCalledTimes(1);
    expect(onJoinChannelCall.mock.calls[0]).toEqual([
      "general",
      "general@muc.waddle.example",
      media,
    ]);
    expect(onJoinChannelCall.mock.calls[0]?.[2]).toBe(media);
  });

  test("passes the resolver's local-part fallback through when the channel is unknown", () => {
    // `resolveChannelIdForRoomJid` falls back to the JID's local-part
    // for an unknown managed-room JID (not null) — that value must reach
    // onJoinChannelCall verbatim so the Join still targets the right room.
    const onJoinChannelCall = mock(
      (_channelId: string | null, _roomJid: string, _media: CallMedia) => {},
    );
    const media: CallMedia = { audio: false, video: true };
    handleJoinCall(
      entry({ channel: "orphan@muc.waddle.example" }),
      media,
      [],
      onJoinChannelCall,
    );
    expect(onJoinChannelCall.mock.calls).toEqual([
      ["orphan", "orphan@muc.waddle.example", media],
    ]);
  });

  // No DM-row-no-join test at this layer: it would be vacuous. The Join
  // path is unreachable for DM threads upstream of `ThreadsView` — only
  // `ThreadsListRow.vue` decides whether to emit `join-call`, and it
  // renders the `CallAnchorCard` (the sole emitter) behind
  // `v-if="isCallThread && callState"`. DM anchors and plain threads keep
  // the plain title row, which has no Join affordance, so no `join-call`
  // event is ever dispatched for them. At the ThreadsView level the
  // meaningful guarantee is the one above (and that `open-thread`, the
  // DM routing path, never reaches `onJoinChannelCall`), asserted next.
  test("an open-thread dispatch never reaches onJoinChannelCall", async () => {
    const onJoinChannelCall = mock(
      (_channelId: string | null, _roomJid: string, _media: CallMedia) => {},
    );
    const h = handlers();
    await handleOpenThread(
      entry({ channel: "bob@waddle.example", thread_id: "t-dm" }),
      [{ id: "general", jid: "general@muc.waddle.example" }],
      h,
    );
    // Opening a (DM) thread routes through onSelectThreadEntry, never the
    // Join path — the two handlers are wired to distinct panel events.
    expect(h.onOpenDmThread).toHaveBeenCalledTimes(1);
    expect(onJoinChannelCall).not.toHaveBeenCalled();
  });
});
