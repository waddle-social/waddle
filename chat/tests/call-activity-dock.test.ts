import { afterEach, describe, expect, mock, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createSSRApp, h, nextTick, shallowReactive } from "vue";
import { parse, compileScript } from "vue/compiler-sfc";
import { renderToString } from "vue/server-renderer";
import ts from "typescript";
import {
  buildCallActivityDockEntries,
  callActivityDockAction,
  callActivityDockSelection,
  sortCallActivityDockEntries,
} from "../src/lib/calls/call-activity-dock";
import {
  activeChannelRailCallCount,
  activeDmRailCallCount,
} from "../src/lib/calls/call-rail-counts";
import {
  $callState,
  clearCallState,
} from "../src/lib/calls/call-store";
import {
  $callCamEnabled,
  $callConnecting,
  $callMicEnabled,
} from "../src/lib/calls/call-controls";
import { $callUiMode } from "../src/lib/calls/ui-mode";
import { applyMucCallPresence, clearMucCallParticipants, $mucCallMedia, $mucCallParticipants } from "../src/lib/calls/muc-call-presence";
import {
  clearAllMucCallSessionCacheForTests,
  markMucCallSessionTerminatePending,
} from "../src/lib/calls/muc-call-session-cache";
import { clearDmCallActivities, $dmCallActivities } from "../src/lib/calls/dm-call-activity";
import type { DmCallActivity } from "../src/lib/calls/dm-call-activity";
import type { CallState, LiveKitJoin } from "../src/lib/calls/types";
import { connectionStore } from "../src/lib/connection-store";

afterEach(() => {
  clearCallState();
  clearMucCallParticipants();
  clearAllMucCallSessionCacheForTests();
  clearDmCallActivities();
  $callConnecting.set(false);
  $callMicEnabled.set(true);
  $callCamEnabled.set(true);
  $callUiMode.set("split");
  connectionStore.client = null;
  connectionStore.selfFullJid = null;
  connectionStore.session = null;
});

const liveKitJoin: LiveKitJoin = {
  url: "wss://livekit.example.test",
  room: "call-room",
  identity: "alice@example.com/web",
  token: "token",
};

function liveKitJoinFor(identity = "alice@example.com/web"): LiveKitJoin {
  return {
    url: "wss://livekit.example.test",
    room: "call-room",
    identity,
    token: jwtWithExp(4_102_444_800),
  };
}

function jwtWithExp(exp: number): string {
  return [
    base64Url(JSON.stringify({ alg: "none", typ: "JWT" })),
    base64Url(JSON.stringify({ exp })),
    "sig",
  ].join(".");
}

function base64Url(value: string): string {
  return btoa(value).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

describe("call activity dock model", () => {
  test("surfaces group calls and DM calls across sidebar modes", () => {
    const dmActivity: DmCallActivity = {
      peerJid: "bob@example.com",
      sid: "dm-call-1",
      media: { audio: true, video: true },
      state: "accepted",
      direction: "incoming",
      updatedAt: "2026-05-25T12:00:00.000Z",
    };

    const entries = buildCallActivityDockEntries({
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
        { id: "design", name: "Design", jid: "design@conference.example.com" },
      ],
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob" },
      ],
      activeChannelId: "general",
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(["general@conference.example.com"]),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {
        "general@conference.example.com": 3,
      },
      callParticipants: {
        "general@conference.example.com": ["alice", "bob", "carol"],
      },
      dmCallActivities: {
        "bob@example.com": dmActivity,
      },
    });

    expect(entries).toEqual([
      {
        kind: "channel",
        key: "channel:general",
        channelId: "general",
        roomJid: "general@conference.example.com",
        title: "General",
        participantCount: 3,
        participantLabels: ["alice", "bob", "carol"],
        media: { audio: true, video: false },
        isKnownChannel: true,
        isActive: true,
      },
      {
        kind: "dm",
        key: "dm:bob@example.com:dm-call-1",
        peerJid: "bob@example.com",
        sid: "dm-call-1",
        title: "Bob",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
        isActive: false,
      },
    ]);
  });

  test("surfaces group-DM room calls while the DM sidebar is active", () => {
    const entries = buildCallActivityDockEntries({
      channels: [],
      groupDms: [
        { id: "gdm-launch", name: "Launch crew", roomJid: "group-dm-launch@muc.example.com" },
      ],
      conversations: [],
      activeChannelId: "gdm-launch",
      activeChannelRoomJid: "group-dm-launch@muc.example.com",
      activePeerJid: null,
      sidebarMode: "dms",
      activeChannelJids: new Set(["group-dm-launch@muc.example.com"]),
      managedMucDomain: "muc.example.com",
      callParticipantCounts: {
        "group-dm-launch@muc.example.com": 2,
      },
      callParticipants: {
        "group-dm-launch@muc.example.com": ["alice", "bob"],
      },
      callMediaByRoom: {
        "group-dm-launch@muc.example.com": { audio: true, video: true },
      },
      dmCallActivities: {},
    });

    expect(entries).toEqual([
      {
        kind: "channel",
        key: "group-dm:gdm-launch",
        channelId: "gdm-launch",
        roomJid: "group-dm-launch@muc.example.com",
        title: "Launch crew",
        participantCount: 2,
        participantLabels: ["alice", "bob"],
        media: { audio: true, video: true },
        isKnownChannel: true,
        isActive: true,
        isGroupDm: true,
      },
    ]);
    expect(callActivityDockSelection(entries[0], { phase: "idle" }, "alice@example.com/web")).toEqual({
      kind: "group-dm-join",
      roomJid: "group-dm-launch@muc.example.com",
      media: { audio: true, video: true },
    });
    expect(callActivityDockSelection(entries[0], {
      phase: "outgoing",
      to: "bob@example.com",
      sid: "dm-call",
      media: { audio: true, video: false },
    }, "alice@example.com/web")).toEqual({
      kind: "group-dm",
      roomJid: "group-dm-launch@muc.example.com",
    });
  });

  test("falls back to peer localpart and orders DM calls by recency", () => {
    const entries = buildCallActivityDockEntries({
      channels: [],
      conversations: [],
      activeChannelId: null,
      activePeerJid: "carol@example.com",
      sidebarMode: "dms",
      activeChannelJids: new Set(),
      callParticipantCounts: {},
      dmCallActivities: {
        "bob@example.com": {
          peerJid: "bob@example.com",
          sid: "older",
          media: { audio: true, video: false },
          state: "ringing",
          direction: "outgoing",
          updatedAt: "2026-05-25T11:00:00.000Z",
        },
        "carol@example.com": {
          peerJid: "carol@example.com",
          sid: "newer",
          media: { audio: true, video: false },
          state: "accepted",
          direction: "unknown",
          updatedAt: "2026-05-25T12:00:00.000Z",
        },
      },
    });

    expect(entries.map((entry) => entry.title)).toEqual(["carol", "bob"]);
    expect(entries[0]).toMatchObject({
      kind: "dm",
      peerJid: "carol@example.com",
      isActive: true,
    });
  });

  test("sorts active-call hub entries by user urgency after refresh", () => {
    const entries = buildCallActivityDockEntries({
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob" },
        { peerJid: "carol@example.com", peerUsername: "Carol" },
        { peerJid: "dana@example.com", peerUsername: "Dana" },
      ],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {
        "general@conference.example.com": 3,
      },
      dmCallActivities: {
        "bob@example.com": {
          peerJid: "bob@example.com",
          remoteFullJid: "bob@example.com/phone",
          sid: "incoming",
          media: { audio: true, video: false },
          state: "ringing",
          direction: "incoming",
          updatedAt: "2026-05-25T10:00:00.000Z",
        },
        "carol@example.com": {
          peerJid: "carol@example.com",
          remoteFullJid: "carol@example.com/phone",
          sid: "reconnect",
          media: { audio: true, video: true },
          join: liveKitJoinFor(),
          state: "accepted",
          direction: "incoming",
          updatedAt: "2026-05-25T12:00:00.000Z",
        },
        "dana@example.com": {
          peerJid: "dana@example.com",
          sid: "syncing",
          media: { audio: true, video: false },
          mediaKnown: false,
          state: "accepted",
          direction: "unknown",
          updatedAt: "2026-05-25T13:00:00.000Z",
        },
      },
    });

    const sorted = sortCallActivityDockEntries(entries, { phase: "idle" }, "alice@example.com/web");

    expect(sorted.map((entry) => entry.key)).toEqual([
      "dm:bob@example.com:incoming",
      "dm:carol@example.com:reconnect",
      "channel:general",
      "dm:dana@example.com:syncing",
    ]);
  });

  test("maps active rows to direct join, return, or reconnect actions", () => {
    const entries = buildCallActivityDockEntries({
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob" },
        { peerJid: "carol@example.com", peerUsername: "Carol" },
      ],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "dms",
      activeChannelJids: new Set(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      dmCallActivities: {
        "bob@example.com": {
          peerJid: "bob@example.com",
          sid: "live",
          media: { audio: true, video: true },
          remoteFullJid: "bob@example.com/phone",
          join: liveKitJoinFor(),
          state: "accepted",
          direction: "incoming",
          updatedAt: "2026-05-25T12:00:00.000Z",
        },
        "carol@example.com": {
          peerJid: "carol@example.com",
          sid: "ring",
          media: { audio: true, video: false },
          state: "ringing",
          direction: "incoming",
          updatedAt: "2026-05-25T11:00:00.000Z",
        },
      },
    });

    expect(entries.map((entry) => callActivityDockSelection(entry, { phase: "idle" }, "alice@example.com/web"))).toEqual([
      {
        kind: "channel-join",
        channelId: "general",
        roomJid: "general@conference.example.com",
        media: { audio: true, video: false },
      },
      {
        kind: "dm-reconnect",
        peerJid: "bob@example.com",
        media: { audio: true, video: true },
      },
      {
        kind: "dm-open",
        peerJid: "carol@example.com",
      },
    ]);
    expect(callActivityDockAction(entries[0], {
      phase: "active",
      kind: "muc",
      peer: "general@conference.example.com",
      sid: "muc-call",
      media: { audio: true, video: false },
      join: { url: "wss://livekit.example.test", room: "general", identity: "alice", token: "token" },
    }, "alice@example.com/web")).toBe("return");
    expect(callActivityDockAction(entries[0], {
      phase: "muc-pending",
      kind: "muc",
      peer: "general@conference.example.com",
      sid: "muc-call",
      media: { audio: true, video: false },
      selfNick: "alice",
      attemptId: "attempt-1",
    }, "alice@example.com/web")).toBe("return");
    expect(callActivityDockSelection(entries[0], {
      phase: "outgoing",
      to: "bob@example.com",
      sid: "dm-call",
      media: { audio: true, video: false },
    }, "alice@example.com/web")).toEqual({
      kind: "channel",
      channelId: "general",
      roomJid: "general@conference.example.com",
    });
    expect(callActivityDockAction(entries[1], {
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/desktop",
      sid: "live",
      media: { audio: true, video: true },
      join: { url: "wss://livekit.example.test", room: "dm", identity: "alice", token: "token" },
    }, "alice@example.com/web")).toBe("return");
    expect(callActivityDockSelection(entries[1], {
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/desktop",
      sid: "live",
      media: { audio: true, video: true },
      join: { url: "wss://livekit.example.test", room: "dm", identity: "alice", token: "token" },
    }, "alice@example.com/web")).toEqual({
      kind: "dm-open",
      peerJid: "bob@example.com",
    });
    expect(callActivityDockAction(entries[1], {
      phase: "outgoing",
      to: "dana@example.com",
      sid: "other-call",
      media: { audio: true, video: false },
    }, "alice@example.com/web")).toBe("open");
  });

  test("maps advertised Muji video media into group-call join selections", () => {
    const entries = buildCallActivityDockEntries({
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      callMediaByRoom: {
        "general@conference.example.com": { audio: true, video: true },
      },
      dmCallActivities: {},
    });

    expect(entries[0]).toMatchObject({
      kind: "channel",
      media: { audio: true, video: true },
    });
    expect(callActivityDockSelection(entries[0], { phase: "idle" }, "alice@example.com/web")).toEqual({
      kind: "channel-join",
      channelId: "general",
      roomJid: "general@conference.example.com",
      media: { audio: true, video: true },
    });
  });

  test("surfaces group calls while the channel directory is still loading", () => {
    const entries = buildCallActivityDockEntries({
      channels: [],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      dmCallActivities: {},
    });

    expect(entries).toEqual([
      {
        kind: "channel",
        key: "channel:general@conference.example.com",
        channelId: null,
        roomJid: "general@conference.example.com",
        title: "general",
        participantCount: 2,
        participantLabels: [],
        media: { audio: true, video: false },
        isKnownChannel: false,
        isActive: false,
      },
    ]);
  });

  test("resolves id-only known channels through the managed MUC domain", () => {
    const entries = buildCallActivityDockEntries({
      channels: [
        { id: "general", name: "General" },
      ],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(["general@custom-muc.example.test"]),
      managedMucDomain: "custom-muc.example.test",
      callParticipantCounts: {
        "general@custom-muc.example.test": 3,
      },
      dmCallActivities: {},
    });

    expect(entries).toEqual([
      {
        kind: "channel",
        key: "channel:general",
        channelId: "general",
        roomJid: "general@custom-muc.example.test",
        title: "General",
        participantCount: 3,
        participantLabels: [],
        media: { audio: true, video: false },
        isKnownChannel: true,
        isActive: false,
      },
    ]);
  });

  test("resolves known-channel room JIDs from normalized call count keys", () => {
    const entries = buildCallActivityDockEntries({
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {
        "General@Conference.Example.com/alice": 2,
      },
      dmCallActivities: {},
    });

    expect(entries).toEqual([
      {
        kind: "channel",
        key: "channel:general",
        channelId: "general",
        roomJid: "general@conference.example.com",
        title: "General",
        participantCount: 2,
        participantLabels: [],
        media: { audio: true, video: false },
        isKnownChannel: true,
        isActive: false,
      },
    ]);
  });

  test("filters fallback group calls to the managed MUC domain", () => {
    const entries = buildCallActivityDockEntries({
      channels: [],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {
        "general@conference.example.com": 2,
        "general@other-muc.example.com": 4,
      },
      dmCallActivities: {},
    });

    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({
      kind: "channel",
      roomJid: "general@conference.example.com",
      participantCount: 2,
      isKnownChannel: false,
    });
  });

  test("does not surface unmatched fallback group calls without a trusted MUC domain", () => {
    const entries = buildCallActivityDockEntries({
      channels: [],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(),
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      dmCallActivities: {},
    });

    expect(entries).toEqual([]);
  });

  test("marks fallback group call active by room JID instead of an inferred channel ID", () => {
    const entries = buildCallActivityDockEntries({
      channels: [],
      conversations: [],
      activeChannelId: "channel-uuid",
      activeChannelRoomJid: "general@conference.example.com",
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      dmCallActivities: {},
    });

    expect(entries[0]).toMatchObject({
      kind: "channel",
      channelId: null,
      roomJid: "general@conference.example.com",
      isActive: true,
    });
  });
});

describe("CallActivityDock rendering", () => {
  test("renders group and DM call entries from the live stores", async () => {
    $mucCallParticipants.set({
      "general@conference.example.com": ["alice", "bob"],
    });
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-call-1",
        media: { audio: true, video: true },
        remoteFullJid: "bob@example.com/phone",
        join: liveKitJoinFor(),
        state: "accepted",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });

    const html = await renderCallActivityDock({
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob", unreadCount: 0 },
      ],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set<string>(),
      selfFullJid: "alice@example.com/web",
    });

    expect(html).toContain("Active calls");
    expect(html).toContain("General");
    expect(html).toContain("Bob");
    expect(html).toContain("Group call");
    expect(html).toContain("Video call");
    expect(html).toContain("2 people");
    expect(html).toContain("alice, bob");
    expect(html).toContain("Live");
    expect(html).toContain("Join");
    expect(html).toContain("Reconnect");
    expect(html).toContain('aria-label="Join General, Group call · 2 people, alice, bob in call"');
    expect(html).toContain('aria-label="Reconnect Bob, Video call · Live"');
    expect(html).toContain('aria-label="End Bob video call"');
  });

  test("renders discovered group video calls with video join media", async () => {
    $mucCallParticipants.set({
      "general@conference.example.com": ["alice", "bob"],
    });
    $mucCallMedia.set({
      "general@conference.example.com": { audio: true, video: true },
    });

    const html = await renderCallActivityDock({
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(["general@conference.example.com"]),
      managedMucDomain: "conference.example.com",
    });

    expect(html).toContain("Group video call");
    expect(html).toContain('aria-label="Join General, Group video call · 2 people, alice, bob in call"');
  });

  test("lets the activity dock leave a retained local group call after refresh", async () => {
    connectionStore.session = {
      username: "alice",
      jid: "alice@example.com",
    } as typeof connectionStore.session;
    connectionStore.client = {
      fullJid: "alice@example.com/web",
    } as unknown as typeof connectionStore.client;
    applyMucCallPresence({
      from: "general@conference.example.com/alice",
      muc_jid: "alice@example.com/web",
      muji: { preparing: false, active: true },
    });
    applyMucCallPresence({
      from: "general@conference.example.com/bob",
      muc_jid: "bob@example.com/phone",
      muji: { preparing: false, active: true },
    });
    const props = {
      channels: [{ id: "general", name: "General", jid: "general@conference.example.com" }],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(["general@conference.example.com"]),
      managedMucDomain: "conference.example.com",
      selfFullJid: "alice@example.com/web",
    };

    const html = await renderCallActivityDock(props);
    expect(html).toContain('aria-label="Leave General call"');
    expect(html).toContain("Rejoin");

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/CallActivityDock.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "endEntry")({
      kind: "channel",
      key: "channel:general",
      channelId: "general",
      roomJid: "general@conference.example.com",
      title: "General",
      participantCount: 2,
      participantLabels: ["alice", "bob"],
      media: { audio: true, video: false },
      isKnownChannel: true,
      isActive: false,
    });

    expect(emitted).toEqual([
      ["leaveChannelCall", "general@conference.example.com"],
    ]);
  });

  test("activates group-DM call rows through DM-specific join and open events", async () => {
    const props = {
      channels: [],
      groupDms: [
        { id: "gdm-launch", name: "Launch crew", roomJid: "group-dm-launch@muc.example.com" },
      ],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "dms",
      activeChannelJids: new Set(["group-dm-launch@muc.example.com"]),
      managedMucDomain: "muc.example.com",
      callParticipants: {
        "group-dm-launch@muc.example.com": ["alice", "bob"],
      },
      callMediaByRoom: {
        "group-dm-launch@muc.example.com": { audio: true, video: true },
      },
      selfFullJid: "alice@example.com/web",
    };

    const emitted: unknown[][] = [];
    let bindings = await setupVueComponent("../src/components/calls/CallActivityDock.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "selectEntry")(
      setupBindingRefValue<unknown[]>(bindings, "visibleEntries")[0],
    );

    expect(emitted).toEqual([
      ["joinGroupDmCall", "group-dm-launch@muc.example.com", { audio: true, video: true }],
    ]);

    clearCallState();
    $callState.set({
      phase: "outgoing",
      to: "bob@example.com",
      sid: "dm-call",
      media: { audio: true, video: false },
    });
    emitted.length = 0;
    bindings = await setupVueComponent("../src/components/calls/CallActivityDock.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "selectEntry")(
      setupBindingRefValue<unknown[]>(bindings, "visibleEntries")[0],
    );

    expect(emitted).toEqual([
      ["selectGroupDm", "group-dm-launch@muc.example.com"],
    ]);
  });

  test("renders active group calls before channel metadata hydrates", async () => {
    $mucCallParticipants.set({
      "general@conference.example.com": ["alice", "bob"],
    });

    const html = await renderCallActivityDock({
      channels: [],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set<string>(),
      managedMucDomain: "conference.example.com",
    });

    expect(html).toContain("Active calls");
    expect(html).toContain("general");
    expect(html).toContain("Group call · syncing");
    expect(html).toContain("2 people");
    expect(html).toContain("alice, bob");
    expect(html).toContain("Join");
    expect(html).toContain("aria-label=\"Join general, Group call · syncing · 2 people, alice, bob in call\"");
    expect(html).not.toContain("disabled");
  });

  test("routes pre-hydration group-DM fallback calls through DM-specific events", async () => {
    const props = {
      channels: [],
      groupDms: [],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "dms",
      activeChannelJids: new Set<string>(),
      managedMucDomain: "muc.example.com",
      callParticipants: {
        "group-dm-launch@muc.example.com": ["alice", "bob"],
      },
      callMediaByRoom: {
        "group-dm-launch@muc.example.com": { audio: true, video: true },
      },
      selfFullJid: "alice@example.com/web",
    };

    const entries = buildCallActivityDockEntries({
      ...props,
      callParticipantCounts: {
        "group-dm-launch@muc.example.com": 2,
      },
      dmCallActivities: {},
    });
    expect(callActivityDockSelection(entries[0], { phase: "idle" }, "alice@example.com/web")).toEqual({
      kind: "group-dm-join",
      roomJid: "group-dm-launch@muc.example.com",
      media: { audio: true, video: true },
    });

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/CallActivityDock.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "selectEntry")(
      setupBindingRefValue<unknown[]>(bindings, "visibleEntries")[0],
    );

    expect(emitted).toEqual([
      ["joinGroupDmCall", "group-dm-launch@muc.example.com", { audio: true, video: true }],
    ]);
  });

  test("keeps unmatched channel fallback calls on channel events while DMs are open", async () => {
    const props = {
      channels: [],
      groupDms: [],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "dms",
      activeChannelJids: new Set<string>(),
      managedMucDomain: "muc.example.com",
      callParticipants: {
        "general@muc.example.com": ["alice", "bob"],
      },
      selfFullJid: "alice@example.com/web",
    };

    const entries = buildCallActivityDockEntries({
      ...props,
      callParticipantCounts: {
        "general@muc.example.com": 2,
      },
      dmCallActivities: {},
    });
    expect(callActivityDockSelection(entries[0], { phase: "idle" }, "alice@example.com/web")).toEqual({
      kind: "channel-join",
      channelId: null,
      roomJid: "general@muc.example.com",
      media: { audio: true, video: false },
    });

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/CallActivityDock.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "selectEntry")(
      setupBindingRefValue<unknown[]>(bindings, "visibleEntries")[0],
    );

    expect(emitted).toEqual([
      ["joinChannelCall", null, "general@muc.example.com", { audio: true, video: false }],
    ]);
  });

  test("keeps group calls visible when the desktop DM dock suppresses DM rows", async () => {
    $mucCallParticipants.set({
      "general@conference.example.com": ["alice", "bob", "carol"],
    });
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-call-1",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });

    const html = await renderCallActivityDock({
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob", unreadCount: 0 },
      ],
      activeChannelId: null,
      activePeerJid: "bob@example.com",
      sidebarMode: "dms",
      activeChannelJids: new Set<string>(),
      showDmCalls: false,
    });

    expect(html).toContain("General");
    expect(html).toContain("alice, bob +1");
    expect(html).toContain('aria-label="Join General, Group call · 3 people, alice, bob +1 in call"');
    expect(html).not.toContain("Bob");
    expect(html).not.toContain("Video call");
  });

  test("renders incoming 1:1 call activity without a false answer affordance", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-call-1",
        media: { audio: true, video: false },
        state: "ringing",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });

    const html = await renderCallActivityDock({
      channels: [],
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob", unreadCount: 0 },
      ],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "dms",
      activeChannelJids: new Set<string>(),
    });

    expect(html).toContain("Bob");
    expect(html).toContain("Voice call");
    expect(html).toContain("Ringing");
    expect(html).toContain("Open");
    expect(html).not.toContain("Answer");
  });

  test("renders hydrated incoming 1:1 calls as answer affordances when the proposer full JID is known", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-call-1",
        media: { audio: true, video: false },
        state: "ringing",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });

    const html = await renderCallActivityDock({
      channels: [],
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob", unreadCount: 0 },
      ],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "dms",
      activeChannelJids: new Set<string>(),
    });

    expect(html).toContain("Bob");
    expect(html).toContain("Voice call");
    expect(html).toContain("Ringing");
    expect(html).toContain("Answer");
    expect(html).toContain('aria-label="Answer Bob, Voice call · Ringing"');
  });

  test("renders a conversation group call banner and emits join", async () => {
    $mucCallParticipants.set({
      "general@conference.example.com": ["alice", "bob"],
    });

    const props = {
      roomJid: "general@conference.example.com",
      channelId: "general",
      channelName: "General",
      dmPeerJid: null,
      dmPeerName: null,
    };
    const html = await renderVueComponent("../src/components/calls/ConversationCallBanner.vue", props);
    expect(html).toContain('role="region"');
    expect(html).toContain('aria-label="General has a live call"');
    expect(html).toContain("General has a live call");
    expect(html).toContain("Live now");
    expect(html).toContain("Voice call");
    expect(html).toContain("Channel");
    expect(html).toContain("2 people connected in this channel.");
    expect(html).toContain("Join call");

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/ConversationCallBanner.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "activateBanner")();

    expect(emitted).toEqual([
      ["joinChannelCall", "general", "general@conference.example.com", { audio: true, video: false }],
    ]);
  });

  test("hides the conversation group banner while this resource is already in that call", async () => {
    $mucCallParticipants.set({
      "general@conference.example.com": ["alice", "bob"],
    });
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: "general@conference.example.com",
      sid: "muc-live",
      media: { audio: true, video: false },
      join: liveKitJoin,
    });

    const html = await renderVueComponent("../src/components/calls/ConversationCallBanner.vue", {
      roomJid: "general@conference.example.com",
      channelId: "general",
      channelName: "General",
      dmPeerJid: null,
      dmPeerName: null,
    });

    expect(html).not.toContain("General has a live call");
  });

  test("lets a hard-refreshed tab leave its own retained group-call presence", async () => {
    connectionStore.session = {
      username: "alice",
      jid: "alice@example.com",
    } as typeof connectionStore.session;
    connectionStore.client = {
      fullJid: "alice@example.com/web",
    } as unknown as typeof connectionStore.client;
    applyMucCallPresence({
      from: "general@conference.example.com/alice",
      muc_jid: "alice@example.com/web",
      muji: { preparing: false, active: true },
    });
    applyMucCallPresence({
      from: "general@conference.example.com/bob",
      muc_jid: "bob@example.com/phone",
      muji: { preparing: false, active: true },
    });

    const props = {
      roomJid: "general@conference.example.com",
      channelId: "general",
      channelName: "General",
      dmPeerJid: null,
      dmPeerName: null,
    };
    const html = await renderVueComponent("../src/components/calls/ConversationCallBanner.vue", props);
    expect(html).toContain("Recovered after refresh");
    expect(html).toContain("This browser is still listed in the call after reload.");
    expect(html).toContain("Rejoin call");
    expect(html).toContain("Leave call");
    expect(html).toContain('aria-label="Leave General call after refresh"');

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/ConversationCallBanner.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "activateBanner")();
    setupBindingFunction(bindings, "activateSecondaryBanner")();

    expect(emitted).toEqual([
      ["joinChannelCall", "general", "general@conference.example.com", { audio: true, video: false }],
      ["leaveChannelCall", "general@conference.example.com"],
    ]);
  });

  test("conversation group banner rejoins advertised video calls as video", async () => {
    applyMucCallPresence({
      from: "general@conference.example.com/alice",
      muc_jid: "alice@example.com/web",
      muji: { preparing: false, active: true, audio: true, video: true },
    });
    const props = {
      roomJid: "general@conference.example.com",
      channelId: "general",
      channelName: "General",
      dmPeerJid: null,
      dmPeerName: null,
    };
    const html = await renderVueComponent("../src/components/calls/ConversationCallBanner.vue", props);
    expect(html).toContain("Video call");

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/ConversationCallBanner.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "activateBanner")();

    expect(emitted).toEqual([
      ["joinChannelCall", "general", "general@conference.example.com", { audio: true, video: true }],
    ]);
  });

  test("keeps the conversation call live region mounted before call activity arrives", async () => {
    const html = await renderVueComponent("../src/components/calls/ConversationCallBanner.vue", {
      roomJid: "general@conference.example.com",
      channelId: "general",
      channelName: "General",
      dmPeerJid: null,
      dmPeerName: null,
    });

    expect(html).toContain('aria-live="polite"');
    expect(html).toContain('aria-atomic="true"');
    expect(html).not.toContain('role="region"');
    expect(html).not.toContain('aria-label="Conversation call status"');
    expect(html).not.toContain("Join call");
    expect(html).not.toContain("General has a live call");
  });

  test("renders a conversation DM accepted-call banner and emits reconnect", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com/phone",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-live",
        media: { audio: true, video: true },
        join: liveKitJoinFor(),
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const props = {
      roomJid: null,
      channelId: null,
      channelName: null,
      dmPeerJid: "bob@example.com",
      dmPeerName: "Bob",
      selfFullJid: "alice@example.com/web",
    };
    const html = await renderVueComponent("../src/components/calls/ConversationCallBanner.vue", props);
    expect(html).toContain("Call with Bob is live");
    expect(html).toContain("Recovered after refresh");
    expect(html).toContain("Video call");
    expect(html).toContain("1:1");
    expect(html).toContain("The video call is still live after reload.");
    expect(html).toContain("Rejoin");
    expect(html).toContain("End call");

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/ConversationCallBanner.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "activateBanner")();
    setupBindingFunction(bindings, "activateSecondaryBanner")();

    expect(emitted).toEqual([
      ["reconnectDm", "bob@example.com", { audio: true, video: true }],
      ["endDm", "bob@example.com", "dm-live"],
    ]);
  });

  test("renders a conversation DM incoming-call banner and emits answer", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com/phone",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-ring",
        media: { audio: true, video: false },
        state: "ringing",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });

    const props = {
      roomJid: null,
      channelId: null,
      channelName: null,
      dmPeerJid: "bob@example.com",
      dmPeerName: "Bob",
    };
    const html = await renderVueComponent("../src/components/calls/ConversationCallBanner.vue", props);
    expect(html).toContain("Incoming call");
    expect(html).toContain("Voice call");
    expect(html).toContain("Ringing");
    expect(html).toContain("Bob is calling");
    expect(html).toContain("Bob is calling with a voice call.");
    expect(html).toContain("Answer");

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/ConversationCallBanner.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "activateBanner")();

    expect(emitted).toEqual([
      ["answerDm", "bob@example.com", "bob@example.com/phone", "dm-ring", { audio: true, video: false }],
    ]);
  });

  test("keeps unavailable conversation DM banners passive", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com/phone",
        sid: "dm-live",
        media: { audio: true, video: false },
        mediaKnown: false,
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const props = {
      roomJid: null,
      channelId: null,
      channelName: null,
      dmPeerJid: "bob@example.com",
      dmPeerName: "Bob",
    };
    const html = await renderVueComponent("../src/components/calls/ConversationCallBanner.vue", props);
    expect(html).toContain("Call with Bob is live");
    expect(html).toContain("Syncing");
    expect(html).toContain("Details pending");
    expect(html).toContain("Reconnect details are not available on this tab yet.");
    expect(html).toContain("Unavailable");
    expect(html).toContain("disabled");

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/ConversationCallBanner.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "activateBanner")();
    expect(emitted).toEqual([]);
  });

  test("labels accepted DM calls that belong to another resource as unavailable here", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com/phone",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-live-other-resource",
        media: { audio: true, video: false },
        join: liveKitJoinFor("alice@example.com/phone"),
        state: "accepted",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });

    const html = await renderVueComponent("../src/components/calls/ConversationCallBanner.vue", {
      roomJid: null,
      channelId: null,
      channelName: null,
      dmPeerJid: "bob@example.com",
      dmPeerName: "Bob",
      selfFullJid: "alice@example.com/web",
    });

    expect(html).toContain("Other device");
    expect(html).toContain("This call is live on another browser or device.");
    expect(html).toContain("Unavailable");
    expect(html).not.toContain("Waiting for call details to finish syncing.");
  });

  test("lets expired same-resource conversation DM banners still end the call", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com/phone",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-live-expired",
        media: { audio: true, video: true },
        join: {
          ...liveKitJoinFor("alice@example.com/web"),
          token: jwtWithExp(Math.floor(Date.now() / 1000) + 10),
        },
        state: "accepted",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });

    const props = {
      roomJid: null,
      channelId: null,
      channelName: null,
      dmPeerJid: "bob@example.com",
      dmPeerName: "Bob",
      selfFullJid: "alice@example.com/web",
    };
    const html = await renderVueComponent("../src/components/calls/ConversationCallBanner.vue", props);

    expect(html).toContain("Recovered after refresh");
    expect(html).toContain("The saved reconnect details for this video call expired, but this tab can still end the call.");
    expect(html).toContain("Unavailable");
    expect(html).toContain("End call");
    expect(html).not.toContain("Rejoin");

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/ConversationCallBanner.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "activateBanner")();
    setupBindingFunction(bindings, "activateSecondaryBanner")();
    expect(emitted).toEqual([
      ["endDm", "bob@example.com", "dm-live-expired"],
    ]);
  });

  test("suppresses header DM activity controls when the conversation banner owns them", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com/phone",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-ring",
        media: { audio: true, video: false },
        state: "ringing",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });

    const html = await renderVueComponent("../src/components/calls/CallButton.vue", {
      peerBareJid: "bob@example.com",
      showActivityControls: false,
    });

    expect(html).not.toContain("Answer voice call");
    expect(html).not.toContain("Incoming voice call");
    expect(html).not.toContain("Start voice call");
  });

  test("suppresses header MUC start controls when the conversation banner owns the live call", async () => {
    $mucCallParticipants.set({
      "general@conference.example.com": ["alice", "bob"],
    });

    const html = await renderVueComponent("../src/components/calls/MucCallButton.vue", {
      roomJid: "general@conference.example.com",
      showActivePill: false,
      hideStartControlsWhenActiveCall: true,
    });

    expect(html).not.toContain("Start voice call in this channel");
    expect(html).not.toContain("Start video call in this channel");
    expect(html).not.toContain("Live call in this channel");
  });

  test("is mounted in the desktop sidebar and visible mobile shell", () => {
    const readyShell = readFileSync(new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url), "utf8");
    const mobileDrawers = readFileSync(new URL("../src/components/chat/ChatMobileDrawers.vue", import.meta.url), "utf8");
    const callActivityDock = readFileSync(new URL("../src/components/calls/CallActivityDock.vue", import.meta.url), "utf8");
    const contentArea = readFileSync(new URL("../src/components/chat/ContentArea.vue", import.meta.url), "utf8");
    const chatHeader = readFileSync(new URL("../src/components/chat/ChatHeader.vue", import.meta.url), "utf8");
    const callButton = readFileSync(new URL("../src/components/calls/CallButton.vue", import.meta.url), "utf8");
    const mucCallButton = readFileSync(new URL("../src/components/calls/MucCallButton.vue", import.meta.url), "utf8");
    const conversationBanner = readFileSync(new URL("../src/components/calls/ConversationCallBanner.vue", import.meta.url), "utf8");

    expect(mucCallButton).toContain("canResumeMucCallActivity");
    expect(mucCallButton).toContain("tryResumeFirst:");
    expect(mucCallButton).toContain("roomCall.localResourceInCall.value");

    expect(readyShell).toContain("import CallActivityDock");
    expect(readyShell).toContain("import CurrentCallPanel");
    expect(readyShell.match(/<CallActivityDock/g)?.length ?? 0).toBeGreaterThanOrEqual(2);
    expect(readyShell.match(/<CurrentCallPanel/g)?.length ?? 0).toBeGreaterThanOrEqual(2);
    expect(readyShell).toContain("class=\"current-call-panel--mobile\"");
    expect(readyShell).toContain("class=\"call-activity-dock--mobile\"");
    expect(readyShell.match(/hide-current-call/g)?.length ?? 0).toBeGreaterThanOrEqual(3);
    expect(readyShell).toContain(":join-channel-call=\"joinChannelCallFromActivity\"");
    expect(readyShell).toContain(":leave-channel-call=\"leaveRetainedChannelCall\"");
    expect(readyShell).toContain(":answer-dm=\"answerDmFromActivity\"");
    expect(readyShell).toContain(":reconnect-dm=\"reconnectDmFromDock\"");
    expect(readyShell).toContain(":end-dm=\"endRecoveredDmFromActivity\"");
    expect(readyShell).toContain(":active-channel-call-count=\"activeChannelCallCount\"");
    expect(readyShell).toContain(":active-dm-call-count=\"activeDmCallCount\"");
    expect(readyShell).toContain(":call-participants=\"retainedMucCallParticipantsStore\"");
    expect(readyShell).toContain(":call-media-by-room=\"retainedMucCallMediaStore\"");
    expect(readyShell).toContain("@select-channel=\"onSelectChannelFromSidebar\"");
    expect(readyShell).toContain("@select-group-dm=\"selectGroupDm\"");
    expect(readyShell).toContain("@join-channel-call=\"joinChannelCallFromActivity\"");
    expect(readyShell).toContain("@join-group-dm-call=\"joinGroupDmCallFromActivity\"");
    expect(readyShell.match(/@leave-channel-call="leaveRetainedChannelCall"/g)?.length ?? 0).toBeGreaterThanOrEqual(3);
    expect(readyShell).toContain("@answer-dm=\"answerDmFromActivity\"");
    expect(readyShell).toContain("@select-dm=\"selectDm\"");
    expect(readyShell).toContain("@reconnect-dm=\"reconnectDmFromDock\"");
    expect(readyShell).toContain("@end-dm=\"endRecoveredDmFromActivity\"");

    expect(mobileDrawers).not.toContain("CallActivityDock");
    expect(mobileDrawers).toContain("@join-channel-call=\"joinChannelCallFromMobile\"");
    expect(mobileDrawers).toContain("@leave-channel-call=\"leaveChannelCallFromMobile\"");
    expect(mobileDrawers).toContain("@answer-dm=\"answerDmFromMobile\"");
    expect(mobileDrawers).toContain("@reconnect-dm=\"reconnectDmFromMobile\"");
    expect(mobileDrawers).toContain("@end-dm=\"endDmFromMobile\"");
    expect(readyShell).toContain(":call-participants=\"retainedMucCallParticipantsStore\"");
    expect(readyShell).toContain(":active-channel-call-count=\"activeChannelCallCount\"");
    expect(mobileDrawers).toContain(":active-channel-call-count=\"visibleActiveChannelCallCount\"");
    expect(mobileDrawers).toContain(":active-dm-call-count=\"visibleActiveDmCallCount\"");
    expect(mobileDrawers).toContain("visibleCallParticipants");
    expect(mobileDrawers).toContain(":call-participants=\"visibleCallParticipants\"");
    expect(mobileDrawers).toContain(":call-media-by-room=\"visibleCallMediaByRoom\"");
    expect(mobileDrawers).toContain(":managed-muc-domain=\"visibleManagedMucDomain\"");
    expect(mobileDrawers).toContain("@toggle-dms=\"openDmList\"");
    expect(mobileDrawers).toContain("hide-current-call");
    expect(callActivityDock).toContain("entry.peerJid.toLowerCase() === barePeerJid(current.peer).toLowerCase()");

    expect(contentArea).toContain("import ConversationCallBanner");
    expect(contentArea).toContain("<ConversationCallBanner");
    expect(contentArea).toContain(":show-call-active-pill=\"false\"");
    expect(contentArea).toContain(":show-dm-call-activity-controls=\"false\"");
    expect(contentArea).toContain(":hide-muc-start-controls-when-active-call=\"true\"");
    expect(contentArea).toContain("@join-channel-call=\"(channelId, roomJid, media) => emit('joinChannelCall', channelId, roomJid, media)\"");
    expect(contentArea).toContain("@leave-channel-call=\"(roomJid) => emit('leaveChannelCall', roomJid)\"");
    expect(contentArea).toContain("@answer-dm=\"(peerJid, remoteFullJid, sid, media) => emit('answerDm', peerJid, remoteFullJid, sid, media)\"");
    expect(contentArea).toContain("@reconnect-dm=\"(peerJid, media) => emit('reconnectDm', peerJid, media)\"");
    expect(contentArea).toContain("@end-dm=\"(peerJid, sid) => emit('endDm', peerJid, sid)\"");
    expect(readyShell).toContain("@join-channel-call=\"joinChannelCallFromActivity\"");
    expect(readyShell).toContain("@leave-channel-call=\"leaveRetainedChannelCall\"");
    expect(readyShell).toContain("@answer-dm=\"answerDmFromActivity\"");
    expect(readyShell).toContain("@reconnect-dm=\"reconnectDmFromDock\"");
    expect(readyShell).toContain("@end-dm=\"endRecoveredDmFromActivity\"");
    expect(chatHeader).toContain("showCallActivePill?: boolean");
    expect(chatHeader).toContain("showDmCallActivityControls?: boolean");
    expect(chatHeader).toContain("hideMucStartControlsWhenActiveCall?: boolean");
    expect(chatHeader).toContain("showDmCallActivityStatus");
    expect(chatHeader).toContain(":show-activity-controls=\"showDmCallActivityControls !== false\"");
    expect(chatHeader).toContain(":show-active-pill=\"showCallActivePill !== false\"");
    expect(chatHeader).toContain(":hide-start-controls-when-active-call=\"hideMucStartControlsWhenActiveCall\"");
    expect(callButton).toContain("showActivityControls?: boolean");
    expect(callButton).toContain("v-if=\"showPeerCallActivity || showStartControls\"");
    expect(callButton).toContain("v-if=\"showPeerCallActivity\"");
    expect(mucCallButton).toContain("showActivePill?: boolean");
    expect(mucCallButton).toContain("hideStartControlsWhenActiveCall?: boolean");
    expect(mucCallButton).toContain("const roomCall = useRoomHasActiveCall(() => props.roomJid)");
    expect(mucCallButton).toContain("props.hideStartControlsWhenActiveCall && roomCall.hasActiveCall.value");
    expect(mucCallButton).toContain("v-if=\"showActivePill !== false\"");
    expect(conversationBanner).toContain("canEndRecoveredDmCallActivity");
    expect(conversationBanner).not.toContain("function isBusy");
  });

  test("ContentArea forwards conversation call banner events at runtime", async () => {
    const emitted: unknown[][] = [];
    const previousProbe = (globalThis as {
      __waddleConversationCallBannerProbe?: (
        props: Record<string, unknown>,
        emit: (event: string, ...args: unknown[]) => void,
      ) => void;
    }).__waddleConversationCallBannerProbe;
    const media = { audio: true, video: false };
    (globalThis as {
      __waddleConversationCallBannerProbe?: (
        props: Record<string, unknown>,
        emit: (event: string, ...args: unknown[]) => void,
      ) => void;
    }).__waddleConversationCallBannerProbe = (props, emit) => {
      expect(props.roomJid).toBe("general@conference.example.com");
      expect(props.channelId).toBe("general");
      expect(props.dmPeerJid).toBe("bob@example.com");
      emit("joinChannelCall", "general", "general@conference.example.com", media);
      emit("leaveChannelCall", "general@conference.example.com");
      emit("answerDm", "bob@example.com", "bob@example.com/phone", "sid-1", media);
      emit("reconnectDm", "bob@example.com", media);
      emit("endDm", "bob@example.com", "sid-2");
    };

    try {
      await suppressVueLifecycleSetupWarnings(() => renderVueComponent("../src/components/chat/ContentArea.vue", {
        ...contentAreaBaseProps(),
        onJoinChannelCall: (...args: unknown[]) => emitted.push(["joinChannelCall", ...args]),
        onLeaveChannelCall: (...args: unknown[]) => emitted.push(["leaveChannelCall", ...args]),
        onAnswerDm: (...args: unknown[]) => emitted.push(["answerDm", ...args]),
        onReconnectDm: (...args: unknown[]) => emitted.push(["reconnectDm", ...args]),
        onEndDm: (...args: unknown[]) => emitted.push(["endDm", ...args]),
      }));
    } finally {
      (globalThis as {
        __waddleConversationCallBannerProbe?: (
          props: Record<string, unknown>,
          emit: (event: string, ...args: unknown[]) => void,
        ) => void;
      }).__waddleConversationCallBannerProbe = previousProbe;
    }

    expect(emitted).toEqual([
      ["joinChannelCall", "general", "general@conference.example.com", media],
      ["leaveChannelCall", "general@conference.example.com"],
      ["answerDm", "bob@example.com", "bob@example.com/phone", "sid-1", media],
      ["reconnectDm", "bob@example.com", media],
      ["endDm", "bob@example.com", "sid-2"],
    ]);
  });

  test("renders the persistent current-call panel for an active DM call", async () => {
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-live",
      media: { audio: true, video: false },
      join: liveKitJoin,
    });
    $callMicEnabled.set(false);

    const html = await renderVueComponent("../src/components/calls/CurrentCallPanel.vue", {
      channels: [],
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob" },
      ],
      activeChannelId: null,
      activeChannelRoomJid: null,
      activePeerJid: null,
    });

    expect(html).toContain('aria-label="Current call"');
    expect(html).toContain("Bob");
    expect(html).toContain("Connected");
    expect(html).toContain("Voice call");
    expect(html).toContain("1:1");
    expect(html).toContain("Return");
    expect(html).toContain('aria-label="Unmute microphone"');
    expect(html).not.toContain("aria-pressed");
    expect(html).toContain('aria-label="Hang up current call"');
  });

  test("renders the persistent current-call panel for a connecting group call", async () => {
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: "general@conference.example.com",
      sid: "muc-live",
      media: { audio: true, video: true },
      join: liveKitJoin,
      selfNick: "alice",
    });
    $callConnecting.set(true);
    $mucCallParticipants.set({
      "general@conference.example.com": ["alice", "bob"],
    });

    const html = await renderVueComponent("../src/components/calls/CurrentCallPanel.vue", {
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      conversations: [],
      activeChannelId: "general",
      activeChannelRoomJid: "general@conference.example.com",
      activePeerJid: null,
    });

    expect(html).toContain("General");
    expect(html).toContain("Connecting...");
    expect(html).toContain("Group call");
    expect(html).toContain("alice, bob");
    expect(html).toContain("Viewing");
    expect(html).toContain('aria-label="Viewing General call"');
    expect(html).toContain("disabled");
  });

  test("renders the local group-call nick before Muji participant replay reaches the current panel", async () => {
    $callState.set({
      phase: "muc-pending",
      kind: "muc",
      peer: "general@conference.example.com",
      sid: "muc-live",
      media: { audio: true, video: false },
      selfNick: "alice",
      attemptId: "attempt-1",
    });

    const html = await renderVueComponent("../src/components/calls/CurrentCallPanel.vue", {
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      conversations: [],
      activeChannelId: null,
      activeChannelRoomJid: null,
      activePeerJid: null,
    });

    expect(html).toContain("Starting call");
    expect(html).toContain("Group call");
    expect(html).toContain("alice");
    expect(html).not.toContain("Group</span>");
  });

  test("routes the persistent current-call panel back to the owning conversation", async () => {
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-live",
      media: { audio: true, video: false },
      join: liveKitJoin,
    });
    $callUiMode.set("expanded");
    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/CurrentCallPanel.vue", {
      channels: [],
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob" },
      ],
      activeChannelId: null,
      activeChannelRoomJid: null,
      activePeerJid: null,
    }, (...args) => {
      emitted.push(args);
    });

    setupBindingFunction(bindings, "returnToCall")();

    expect(emitted).toEqual([["selectDm", "bob@example.com"]]);
    expect($callUiMode.get()).toBe("split");
  });

  test("keeps the viewing chip passive when already on the current call", async () => {
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-live",
      media: { audio: true, video: false },
      join: liveKitJoin,
    });
    $callUiMode.set("expanded");
    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/CurrentCallPanel.vue", {
      channels: [],
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob" },
      ],
      activeChannelId: null,
      activeChannelRoomJid: null,
      activePeerJid: "bob@example.com",
    }, (...args) => {
      emitted.push(args);
    });

    setupBindingFunction(bindings, "returnToCall")();

    expect(emitted).toEqual([]);
    expect($callUiMode.get()).toBe("expanded");
  });

  test("filters the current local call out of rediscovery lists", async () => {
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-live",
      media: { audio: true, video: true },
      join: liveKitJoin,
    });
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-live",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
      "carol@example.com": {
        peerJid: "carol@example.com",
        sid: "dm-other",
        media: { audio: true, video: false },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:02:00.000Z",
      },
    });

    const dockHtml = await renderVueComponent("../src/components/calls/CallActivityDock.vue", {
      channels: [],
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob" },
        { peerJid: "carol@example.com", peerUsername: "Carol" },
      ],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "dms",
      activeChannelJids: new Set<string>(),
      hideCurrentCall: true,
    });
    const dmHtml = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      activePeerJid: null,
      hideCurrentCall: true,
    });

    expect(dockHtml).toContain("Carol");
    expect(dockHtml).not.toContain("Bob");
    expect(dmHtml).toContain("carol");
    expect(dmHtml).not.toContain("bob");
  });

  test("keeps different same-peer recovered call SIDs visible while the current call is hidden", async () => {
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-current",
      media: { audio: true, video: true },
      join: liveKitJoinFor("alice@example.com/web"),
    });
    $dmCallActivities.set({
      "bob@example.com\u0000dm-current\u0000alice@example.com/web": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-current",
        media: { audio: true, video: true },
        join: liveKitJoinFor("alice@example.com/web"),
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
      "bob@example.com\u0000dm-orphan\u0000alice@example.com/web": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/laptop",
        sid: "dm-orphan",
        media: { audio: true, video: false },
        join: liveKitJoinFor("alice@example.com/web"),
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:02:00.000Z",
      },
    });

    const dockHtml = await renderVueComponent("../src/components/calls/CallActivityDock.vue", {
      channels: [],
      conversations: [{ peerJid: "bob@example.com", peerUsername: "Bob" }],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "dms",
      activeChannelJids: new Set<string>(),
      hideCurrentCall: true,
      selfFullJid: "alice@example.com/web",
    });
    const dmHtml = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      activePeerJid: null,
      hideCurrentCall: true,
      selfFullJid: "alice@example.com/web",
    });

    expect(dockHtml).toContain("Bob");
    expect(dockHtml).toContain("Voice call · Live");
    expect(dockHtml).toContain('aria-label="End Bob voice call"');
    expect(dmHtml).toContain("bob");
    expect(dmHtml).toContain("Live voice call");
    expect(dmHtml).toContain('aria-label="End bob voice call"');
  });

  test("announces no other calls when the current call is filtered out", async () => {
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-live",
      media: { audio: true, video: true },
      join: liveKitJoin,
    });
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-live",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/calls/CallActivityDock.vue", {
      channels: [],
      conversations: [{ peerJid: "bob@example.com", peerUsername: "Bob" }],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "dms",
      activeChannelJids: new Set<string>(),
      hideCurrentCall: true,
    });

    expect(html).toContain("No other active calls.");
    expect(html).not.toContain('aria-label="Active calls"');
  });

  test("announces no other calls while the current call has not echoed into activity yet", async () => {
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-live",
      media: { audio: true, video: true },
      join: liveKitJoinFor(),
    });

    const html = await renderVueComponent("../src/components/calls/CallActivityDock.vue", {
      channels: [],
      conversations: [{ peerJid: "bob@example.com", peerUsername: "Bob" }],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "dms",
      activeChannelJids: new Set<string>(),
      hideCurrentCall: true,
    });

    expect(html).toContain("No other active calls.");
    expect(html).not.toContain("No active calls.");
    expect(html).not.toContain('aria-label="Active calls"');
  });

  test("labels non-resumable accepted DM calls truthfully in the activity dock", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-other-device",
        media: { audio: true, video: true },
        join: liveKitJoinFor("alice@example.com/phone"),
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/calls/CallActivityDock.vue", {
      channels: [],
      conversations: [{ peerJid: "bob@example.com", peerUsername: "Bob" }],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set<string>(),
      selfFullJid: "alice@example.com/web",
    });

    expect(html).toContain("Video call · Other device");
    expect(html).toContain('aria-label="Open Bob, Video call · Other device"');
    expect(html).not.toContain("Video call · Live");
  });

  test("keeps expired same-resource DM calls endable in the dock", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-expired",
        media: { audio: true, video: true },
        join: {
          ...liveKitJoinFor("alice@example.com/web"),
          token: jwtWithExp(Math.floor(Date.now() / 1000) + 10),
        },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/calls/CallActivityDock.vue", {
      channels: [],
      conversations: [{ peerJid: "bob@example.com", peerUsername: "Bob" }],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set<string>(),
      selfFullJid: "alice@example.com/web",
    });

    expect(html).toContain("Video call · End available");
    expect(html).toContain('aria-label="Open Bob, Video call · End available"');
    expect(html).toContain('aria-label="End Bob video call"');
    expect(html).not.toContain("Reconnect Bob");
  });

  test("expands the persistent current-call panel into a known group channel", async () => {
    $callState.set({
      phase: "active",
      kind: "muc",
      peer: "general@conference.example.com",
      sid: "muc-live",
      media: { audio: true, video: true },
      join: liveKitJoin,
      selfNick: "alice",
    });
    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/calls/CurrentCallPanel.vue", {
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      conversations: [],
      activeChannelId: null,
      activeChannelRoomJid: null,
      activePeerJid: null,
    }, (...args) => {
      emitted.push(args);
    });

    setupBindingFunction(bindings, "expandCall")();

    expect(emitted).toEqual([["selectChannel", "general", "general@conference.example.com"]]);
    expect($callUiMode.get()).toBe("expanded");
  });

  test("renders rail-level active call indicators for spaces and DMs", async () => {
    const html = await renderVueComponent("../src/components/chat/WaddlesSidebar.vue", {
      waddles: [],
      activeSpaceId: null,
      activeSidebarMode: "channels",
      activePage: "chat",
      hasUnreadDms: true,
      activeChannelCallCount: 2,
      activeDmCallCount: 1,
      session: null,
    });

    expect(html).toContain('aria-label="Spaces, 2 active calls"');
    expect(html).toContain('title="Spaces, 2 active calls"');
    expect(html).toContain('aria-label="Direct messages, unread messages, 1 active call"');
    expect(html).toContain('title="Direct messages, unread messages, 1 active call"');
    expect(html).toContain('title="2 active calls"');
    expect(html).toContain('title="1 active call"');
  });

  test("counts local current calls in the rail before discovery echoes activity", () => {
    expect(activeChannelRailCallCount({}, {
      phase: "muc-pending",
      kind: "muc",
      peer: "General@Conference.Example.com/alice",
      sid: "muc-call",
      media: { audio: true, video: false },
      selfNick: "alice",
      attemptId: "attempt-1",
    })).toBe(1);
    expect(activeChannelRailCallCount({
      "general@conference.example.com": 2,
    }, {
      phase: "active",
      kind: "muc",
      peer: "general@conference.example.com",
      sid: "muc-call",
      media: { audio: true, video: false },
      join: liveKitJoin,
      selfNick: "alice",
    })).toBe(1);
    expect(activeDmRailCallCount({}, {
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-call",
      media: { audio: true, video: true },
      join: liveKitJoin,
    })).toBe(1);
    expect(activeDmRailCallCount({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-call",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    }, {
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-call",
      media: { audio: true, video: true },
      join: liveKitJoin,
    })).toBe(1);
  });

  test("renders refreshed call-only DMs in the direct-message panel", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-call-1",
        media: { audio: true, video: true },
        join: liveKitJoinFor(),
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });
    connectionStore.client = { fullJid: "alice@example.com/web" } as unknown as typeof connectionStore.client;

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      activePeerJid: null,
      selfFullJid: "alice@example.com/web",
    });

    expect(html).not.toContain("No conversations yet");
    expect(html).toContain("Active calls");
    expect(html).toContain("bob");
    expect(html).toContain("Live now");
    expect(html).toContain("Live video call");
    expect(html).toContain("Reconnect bob call, Live now, Live video call");
    expect(html).toContain("Live");
  });

  test("renders active group-DM call indicators in the direct-message panel", async () => {
    $mucCallParticipants.set({
      "group-dm-launch@muc.example.com": ["alice", "bob"],
    });
    $mucCallMedia.setKey("group-dm-launch@muc.example.com", { audio: true, video: true });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      groupDms: [
        { id: "gdm-launch", name: "Launch crew", roomJid: "group-dm-launch@muc.example.com" },
      ],
      activePeerJid: null,
      activeGroupDmRoomJid: null,
    });

    expect(html).toContain("Launch crew");
    expect(html).toContain("Video call");
    expect(html).toContain('aria-label="Open group message Launch crew, Video call live, 2 people"');
  });

  test("orders direct-call panel rows by action urgency before recency", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "incoming",
        media: { audio: true, video: false },
        state: "ringing",
        direction: "incoming",
        updatedAt: "2026-05-25T09:00:00.000Z",
      },
      "carol@example.com": {
        peerJid: "carol@example.com",
        remoteFullJid: "carol@example.com/phone",
        sid: "reconnect",
        media: { audio: true, video: true },
        join: liveKitJoinFor(),
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [
        { peerJid: "bob@example.com", peerUsername: "Bob" },
        { peerJid: "carol@example.com", peerUsername: "Carol" },
      ],
      activePeerJid: null,
      selfFullJid: "alice@example.com/web",
    });

    const incomingIndex = html.indexOf("Answer Bob call, Incoming call, Incoming voice call");
    const reconnectIndex = html.indexOf("Reconnect Carol call, Live now, Live video call");

    expect(incomingIndex).toBeGreaterThanOrEqual(0);
    expect(reconnectIndex).toBeGreaterThanOrEqual(0);
    expect(incomingIndex).toBeLessThan(reconnectIndex);
  });

  test("labels non-resumable direct calls in the direct-message panel", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-call-other-device",
        media: { audio: true, video: false },
        join: liveKitJoinFor("alice@example.com/phone"),
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      activePeerJid: null,
      selfFullJid: "alice@example.com/web",
    });

    expect(html).toContain("Other device");
    expect(html).toContain("Voice call · Other device");
    expect(html).toContain("This call is live on another browser or device.");
    expect(html).toContain("Open bob conversation, Other device, Voice call · Other device");
    expect(html).not.toContain("Reconnect bob call");
  });

  test("keeps expired same-resource direct calls endable in the direct-message panel", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-call-expired",
        media: { audio: true, video: true },
        join: {
          ...liveKitJoinFor("alice@example.com/web"),
          token: jwtWithExp(Math.floor(Date.now() / 1000) + 10),
        },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      activePeerJid: null,
      selfFullJid: "alice@example.com/web",
    });

    expect(html).toContain("Recovered after refresh");
    expect(html).toContain("Video call · End available");
    expect(html).toContain("The saved reconnect details expired, but this tab can still end the call.");
    expect(html).toContain("Open bob conversation, Recovered after refresh, Video call · End available");
    expect(html).toContain('aria-label="End bob video call"');
    expect(html).not.toContain("Reconnect bob call");
  });

  test("does not duplicate refreshed DM call activity when the conversation already exists", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "Bob@Example.com/laptop",
        sid: "dm-call-1",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [{
        peerJid: "bob@example.com/phone",
        peerUsername: "Bobby",
        lastMessageBody: "Existing chat preview",
        lastMessageAt: "2026-05-25T11:00:00.000Z",
        unreadCount: 0,
      }],
      activePeerJid: "BOB@example.com/tablet",
    });

    expect(html).toContain("Active calls");
    expect(html).toContain("Bobby");
    expect(html).toContain("Syncing");
    expect(html).toContain("Video call · Details pending");
    expect(html).toContain("Reconnect details are not available on this tab yet.");
    expect(html).toContain('aria-current="page"');
    expect(html).not.toContain("Existing chat preview");
    expect(html).not.toContain("Video call live");
  });

  test("labels the hidden current call separately in the direct-message panel", async () => {
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-call-current",
      media: { audio: true, video: true },
      join: liveKitJoinFor(),
    });
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com/phone",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-call-current",
        media: { audio: true, video: true },
        join: liveKitJoinFor(),
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [{
        peerJid: "bob@example.com",
        peerUsername: "Bobby",
        lastMessageBody: "Existing chat preview",
        unreadCount: 0,
      }],
      activePeerJid: null,
      selfFullJid: "alice@example.com/web",
      hideCurrentCall: true,
    });

    expect(html).toContain("No other active direct calls.");
    expect(html).toContain("Current call shown separately");
    expect(html).toContain("Current call");
    expect(html).toContain('aria-label="Bobby, not selected, Current call shown separately, Open conversation"');
    expect(html).not.toContain("Return call");
    expect(html).not.toContain("Reconnect call");
  });

  test("announces no other direct calls while the current call has not echoed into activity yet", async () => {
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-call-current",
      media: { audio: true, video: true },
      join: liveKitJoinFor(),
    });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [{
        peerJid: "bob@example.com",
        peerUsername: "Bobby",
        lastMessageBody: "Existing chat preview",
        unreadCount: 0,
      }],
      activePeerJid: null,
      selfFullJid: "alice@example.com/web",
      hideCurrentCall: true,
    });

    expect(html).toContain("No other active direct calls.");
    expect(html).not.toContain("No active direct calls.");
  });

  test("renders outgoing refreshed DM call rows with clear ringing copy", async () => {
    $dmCallActivities.set({
      "alice@example.com": {
        peerJid: "alice@example.com",
        sid: "dm-call-2",
        media: { audio: true, video: false },
        state: "ringing",
        direction: "outgoing",
        updatedAt: "2026-05-25T12:01:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      activePeerJid: null,
    });

    expect(html).toContain("Calling voice call");
    expect(html).toContain("Open alice conversation, Calling, Calling voice call");
    expect(html).not.toContain("Voice call calling");
  });

  test("selects refreshed DM call rows as reconnect or return actions", async () => {
    const activity: DmCallActivity = {
      peerJid: "bob@example.com/phone",
      remoteFullJid: "bob@example.com/phone",
      sid: "dm-call-9",
      media: { audio: true, video: true },
      join: liveKitJoinFor(),
      state: "accepted",
      direction: "incoming",
      updatedAt: "2026-05-25T12:00:00.000Z",
    };
    $dmCallActivities.set({ "bob@example.com": activity });
    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      activePeerJid: null,
      selfFullJid: "alice@example.com/web",
    }, (...args) => {
      emitted.push(args);
    });

    setupBindingFunction(bindings, "selectCallActivity")(activity);
    setupBindingFunction(bindings, "endCallActivity")(activity);
    expect(emitted).toEqual([
      ["reconnectDm", "bob@example.com", { audio: true, video: true }],
      ["endDm", "bob@example.com", "dm-call-9"],
    ]);

    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/laptop",
      sid: "dm-call-9",
      media: { audio: true, video: true },
      join: { url: "wss://livekit.example.test", room: "dm", identity: "alice", token: "token" },
    });
    const activeEmitted: unknown[][] = [];
    const activeBindings = await setupVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      activePeerJid: "bob@example.com",
    }, (...args) => {
      activeEmitted.push(args);
    });

    setupBindingFunction(activeBindings, "selectCallActivity")(activity);
    expect(activeEmitted).toEqual([
      ["selectDm", "bob@example.com"],
    ]);
  });

  test("selects hydrated incoming DM call rows as answer actions", async () => {
    const activity: DmCallActivity = {
      peerJid: "bob@example.com/phone",
      remoteFullJid: "bob@example.com/phone",
      sid: "dm-call-10",
      media: { audio: true, video: false },
      state: "ringing",
      direction: "incoming",
      updatedAt: "2026-05-25T12:00:00.000Z",
    };
    $dmCallActivities.set({ "bob@example.com": activity });
    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      activePeerJid: null,
    }, (...args) => {
      emitted.push(args);
    });

    setupBindingFunction(bindings, "selectCallActivity")(activity);
    expect(emitted).toEqual([
      ["answerDm", "bob@example.com", "bob@example.com/phone", "dm-call-10", { audio: true, video: false }],
    ]);
  });

  test("orders refreshed call-only DMs with real conversations by recency", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "Bob@example.com/laptop",
        sid: "dm-call-3",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
      "alice@example.com": {
        peerJid: "alice@example.com",
        sid: "dm-call-4",
        media: { audio: true, video: false },
        state: "ringing",
        direction: "outgoing",
        updatedAt: "2026-05-25T11:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [{
        peerJid: "carol@example.com",
        peerUsername: "Carol",
        lastMessageBody: "Latest real conversation",
        lastMessageAt: "2026-05-25T12:05:00.000Z",
        unreadCount: 0,
      }],
      activePeerJid: null,
    });

    const bobIndex = html.indexOf("Video call · Details pending");
    const aliceIndex = html.indexOf("Calling voice call", bobIndex);
    const carolIndex = html.indexOf("Latest real conversation", aliceIndex);
    expect(bobIndex).toBeGreaterThanOrEqual(0);
    expect(aliceIndex).toBeGreaterThanOrEqual(0);
    expect(carolIndex).toBeGreaterThanOrEqual(0);
    expect(bobIndex).toBeLessThan(aliceIndex);
    expect(aliceIndex).toBeLessThan(carolIndex);
  });

  test("uses call activity recency when sorting an existing refreshed DM conversation", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com/laptop",
        sid: "dm-call-5",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:10:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [
        {
          peerJid: "bob@example.com/phone",
          peerUsername: "Bobby",
          lastMessageBody: "Old message, active call",
          lastMessageAt: "2026-05-25T10:00:00.000Z",
          unreadCount: 0,
        },
        {
          peerJid: "carol@example.com",
          peerUsername: "Carol",
          lastMessageBody: "Newer text-only conversation",
          lastMessageAt: "2026-05-25T12:05:00.000Z",
          unreadCount: 0,
        },
      ],
      activePeerJid: null,
    });

    const bobbyIndex = html.indexOf("Bobby");
    const carolIndex = html.indexOf("Carol");
    expect(bobbyIndex).toBeGreaterThanOrEqual(0);
    expect(carolIndex).toBeGreaterThanOrEqual(0);
    expect(bobbyIndex).toBeLessThan(carolIndex);
    expect(html).not.toContain("Old message, active call");
    expect(html).toContain("Syncing");
    expect(html).toContain("Video call · Details pending");
    expect(html).not.toContain("Video call live");
  });

  test("uses normalized bare JIDs as the equal-recency fallback order", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "Bob@example.com/laptop",
        sid: "dm-call-6",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
      "alice@example.com": {
        peerJid: "alice@example.com/phone",
        sid: "dm-call-7",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      activePeerJid: null,
    });

    const aliceIndex = html.indexOf("alice");
    const bobIndex = html.indexOf("bob");
    expect(aliceIndex).toBeGreaterThanOrEqual(0);
    expect(bobIndex).toBeGreaterThanOrEqual(0);
    expect(aliceIndex).toBeLessThan(bobIndex);
  });

  test("uses normalized bare JIDs as the existing-conversation fallback order", async () => {
    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [
        {
          peerJid: "Same@example.com/a",
          peerUsername: "Same Upper",
          lastMessageBody: "Same bare first",
          lastMessageAt: "2026-05-25T12:00:00.000Z",
          unreadCount: 0,
        },
        {
          peerJid: "same@example.com/z",
          peerUsername: "Same Lower",
          lastMessageBody: "Same bare second",
          lastMessageAt: "2026-05-25T12:00:00.000Z",
          unreadCount: 0,
        },
      ],
      activePeerJid: null,
    });

    const upperIndex = html.indexOf("Same Upper");
    const lowerIndex = html.indexOf("Same Lower");
    expect(upperIndex).toBeGreaterThanOrEqual(0);
    expect(lowerIndex).toBeGreaterThanOrEqual(0);
    expect(upperIndex).toBeLessThan(lowerIndex);
  });

  test("renders unknown-direction refreshed DM call rows with fallback ringing copy", async () => {
    $dmCallActivities.set({
      "casey@example.com": {
        peerJid: "casey@example.com",
        sid: "dm-call-8",
        media: { audio: true, video: true },
        state: "ringing",
        direction: "unknown",
        updatedAt: "2026-05-25T12:02:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [],
      activePeerJid: null,
    });

    expect(html).toContain("Video call ringing");
  });

  test("renders sidebar channel rows as direct join affordances when idle", async () => {
    const html = await renderVueComponent("../src/components/chat/TopicsPanel.vue", {
      ...topicsPanelBaseProps(),
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      managedMucDomain: "conference.example.com",
    });

    expect(html).toContain("Join live call, 2 people");
    expect(html).toContain("General, not selected, call ongoing with 2 participants, click to join call");
  });

  test("sidebar channel rows rejoin video group calls with video enabled", async () => {
    const emitted: unknown[][] = [];
    const channel = { id: "general", name: "General", jid: "general@conference.example.com" };
    const props = {
      ...topicsPanelBaseProps(),
      channels: [channel],
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      callMediaByRoom: {
        "general@conference.example.com": { audio: true, video: true },
      },
      managedMucDomain: "conference.example.com",
    };

    const html = await renderVueComponent("../src/components/chat/TopicsPanel.vue", props);
    expect(html).toContain("Join live video call, 2 people");

    const bindings = await setupVueComponent("../src/components/chat/TopicsPanel.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "selectChannelRow")(channel);

    expect(emitted).toEqual([
      ["joinChannelCall", "general", "general@conference.example.com", { audio: true, video: true }],
    ]);
  });

  test("renders sidebar leave controls for retained local group calls after refresh", async () => {
    connectionStore.session = {
      username: "alice",
      jid: "alice@example.com",
    } as typeof connectionStore.session;
    connectionStore.client = {
      fullJid: "alice@example.com/web",
    } as unknown as typeof connectionStore.client;
    applyMucCallPresence({
      from: "general@conference.example.com/alice",
      muc_jid: "alice@example.com/web",
      muji: { preparing: false, active: true },
    });
    applyMucCallPresence({
      from: "general@conference.example.com/bob",
      muc_jid: "bob@example.com/phone",
      muji: { preparing: false, active: true },
    });
    const props = {
      ...topicsPanelBaseProps(),
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      callParticipants: {
        "general@conference.example.com": ["alice", "bob"],
      },
      managedMucDomain: "conference.example.com",
    };

    const html = await renderVueComponent("../src/components/chat/TopicsPanel.vue", props);
    expect(html).toContain("Rejoin live call, 2 people");
    expect(html).toContain('aria-label="Leave General call"');

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/chat/TopicsPanel.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "leaveChannelCall")({
      id: "general",
      name: "General",
      jid: "general@conference.example.com",
    });

    expect(emitted).toEqual([
      ["leaveChannelCall", "general@conference.example.com"],
    ]);
  });

  test("renders refreshed group call rows while channel navigation is loading", async () => {
    const html = await renderVueComponent("../src/components/chat/TopicsPanel.vue", {
      ...topicsPanelBaseProps(),
      isLoading: true,
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      callParticipants: {
        "general@conference.example.com": ["alice", "bob"],
      },
      managedMucDomain: "conference.example.com",
    });

    expect(html).toContain("Active calls");
    expect(html).toContain("general");
    expect(html).toContain("alice, bob in call");
    expect(html).toContain("Join live call, 2 people");
    expect(html).toContain("Join live call, 2 people: alice, bob");
    expect(html).toContain("general, group call syncing, 2 people, alice, bob in call, click to join call");
  });

  test("renders refreshed group video call rows with video join media", async () => {
    const emitted: unknown[][] = [];
    const props = {
      ...topicsPanelBaseProps(),
      isLoading: true,
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      callParticipants: {
        "general@conference.example.com": ["alice", "bob"],
      },
      callMediaByRoom: {
        "general@conference.example.com": { audio: true, video: true },
      },
      managedMucDomain: "conference.example.com",
    };

    const html = await renderVueComponent("../src/components/chat/TopicsPanel.vue", props);
    expect(html).toContain("Join live video call, 2 people");
    expect(html).toContain("general, group video call syncing, 2 people, alice, bob in call, click to join call");

    const bindings = await setupVueComponent("../src/components/chat/TopicsPanel.vue", props, (...args) => {
      emitted.push(args);
    });
    const rows = (bindings.refreshedGroupCallRows as { value: Array<{ roomJid: string; media: CallMedia }> }).value;
    expect(rows[0]?.media).toEqual({ audio: true, video: true });
    setupBindingFunction(bindings, "selectRefreshedGroupCallRow")(rows[0]);

    expect(emitted).toEqual([
      ["joinChannelCall", null, "general@conference.example.com", { audio: true, video: true }],
    ]);
  });

  test("renders refreshed group call leave controls for retained local resources", async () => {
    connectionStore.session = {
      username: "alice",
      jid: "alice@example.com",
    } as typeof connectionStore.session;
    connectionStore.client = {
      fullJid: "alice@example.com/web",
    } as unknown as typeof connectionStore.client;
    applyMucCallPresence({
      from: "general@conference.example.com/alice",
      muc_jid: "alice@example.com/web",
      muji: { preparing: false, active: true },
    });
    const props = {
      ...topicsPanelBaseProps(),
      isLoading: true,
      callParticipantCounts: {
        "general@conference.example.com": 1,
      },
      callParticipants: {
        "general@conference.example.com": ["alice"],
      },
      managedMucDomain: "conference.example.com",
    };

    const html = await renderVueComponent("../src/components/chat/TopicsPanel.vue", props);
    expect(html).toContain("Rejoin live call, 1 person");
    expect(html).toContain('aria-label="Leave general call"');

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/chat/TopicsPanel.vue", props, (...args) => {
      emitted.push(args);
    });
    setupBindingFunction(bindings, "leaveRefreshedGroupCall")({
      key: "group-call:general@conference.example.com",
      roomJid: "general@conference.example.com",
      title: "general",
      participantCount: 1,
      media: { audio: true, video: false },
    });

    expect(emitted).toEqual([
      ["leaveChannelCall", "general@conference.example.com"],
    ]);
  });

  test("does not duplicate refreshed group call rows for known channels", async () => {
    const html = await renderVueComponent("../src/components/chat/TopicsPanel.vue", {
      ...topicsPanelBaseProps(),
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      callParticipants: {
        "general@conference.example.com": ["alice", "bob", "carol"],
      },
      managedMucDomain: "conference.example.com",
    });

    expect(html).toContain("General, not selected, call ongoing with 2 participants, alice, bob +1 in call, click to join call");
    expect(html).toContain("Join live call, 2 people: alice, bob +1");
    expect(html).not.toContain("Group call syncing");
  });

  test("keeps sidebar channel call rows navigation-only while another call is active", async () => {
    $callState.set({
      phase: "outgoing",
      to: "bob@example.com",
      sid: "dm-call",
      media: { audio: true, video: false },
    });

    const html = await renderVueComponent("../src/components/chat/TopicsPanel.vue", {
      ...topicsPanelBaseProps(),
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      managedMucDomain: "conference.example.com",
    });

    expect(html).toContain("Open live call, 2 people");
    expect(html).toContain("General, not selected, call ongoing with 2 participants, click to open call");
    expect(html).not.toContain("click to join call");
  });

  test("renders sidebar channel rows as return affordances while the same room is joining", async () => {
    $callState.set({
      phase: "muc-pending",
      kind: "muc",
      peer: "general@conference.example.com",
      sid: "muc-call",
      media: { audio: true, video: false },
      selfNick: "alice",
      attemptId: "attempt-1",
    });

    const html = await renderVueComponent("../src/components/chat/TopicsPanel.vue", {
      ...topicsPanelBaseProps(),
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com" },
      ],
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      managedMucDomain: "conference.example.com",
    });

    expect(html).toContain("Return live call, 2 people");
    expect(html).toContain("General, not selected, call ongoing with 2 participants, click to return to call");
  });

  test("activates sidebar channel rows through the join, open, and return event paths", async () => {
    expect(await emittedTopicChannelRowEvents()).toEqual([
      ["joinChannelCall", "general", "general@conference.example.com", { audio: true, video: false }],
    ]);

    expect(await emittedTopicChannelRowEvents({
      phase: "outgoing",
      to: "bob@example.com",
      sid: "dm-call",
      media: { audio: true, video: false },
    })).toEqual([
      ["selectChannel", "general", "general@conference.example.com"],
    ]);

    expect(await emittedTopicChannelRowEvents({
      phase: "muc-pending",
      kind: "muc",
      peer: "general@conference.example.com",
      sid: "muc-call",
      media: { audio: true, video: false },
      selfNick: "alice",
      attemptId: "attempt-1",
    })).toEqual([
      ["selectChannel", "general", "general@conference.example.com"],
    ]);
  });

  test("activates sidebar channel rows with normalized call count keys", async () => {
    clearCallState();
    const emitted: unknown[][] = [];
    const channel = { id: "general", name: "General", jid: "general@conference.example.com" };
    const bindings = await setupVueComponent("../src/components/chat/TopicsPanel.vue", {
      ...topicsPanelBaseProps(),
      channels: [channel],
      callParticipantCounts: {
        "General@Conference.Example.com/alice": 2,
      },
      managedMucDomain: "conference.example.com",
    }, (...args) => {
      emitted.push(args);
    });

    setupBindingFunction(bindings, "selectChannelRow")(channel);

    expect(emitted).toEqual([
      ["joinChannelCall", "general", "general@conference.example.com", { audio: true, video: false }],
    ]);
  });

  test("activates refreshed group call rows through the join, open, and return event paths", async () => {
    expect(await emittedRefreshedGroupCallRowEvents()).toEqual([
      ["joinChannelCall", null, "general@conference.example.com", { audio: true, video: false }],
    ]);

    expect(await emittedRefreshedGroupCallRowEvents({
      phase: "outgoing",
      to: "bob@example.com",
      sid: "dm-call",
      media: { audio: true, video: false },
    })).toEqual([
      ["selectChannel", null, "general@conference.example.com"],
    ]);

    expect(await emittedRefreshedGroupCallRowEvents({
      phase: "muc-pending",
      kind: "muc",
      peer: "general@conference.example.com",
      sid: "muc-call",
      media: { audio: true, video: false },
      selfNick: "alice",
      attemptId: "attempt-1",
    })).toEqual([
      ["selectChannel", null, "general@conference.example.com"],
    ]);
  });

  test("mobile drawer forwards sidebar join-call events to the shell handler", async () => {
    const forwarded: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/chat/ChatMobileDrawers.vue", {
      controller: {},
      joinChannelCall: (...args: unknown[]) => {
        forwarded.push(args);
      },
    });

    setupBindingFunction(bindings, "joinChannelCallFromMobile")(
      "general",
      "general@conference.example.com",
      { audio: true, video: false },
    );

    expect(forwarded).toEqual([
      ["general", "general@conference.example.com", { audio: true, video: false }],
    ]);
  });

  test("mobile drawer forwards sidebar leave-call events to the shell handler", async () => {
    const forwarded: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/chat/ChatMobileDrawers.vue", {
      controller: {},
      leaveChannelCall: (...args: unknown[]) => {
        forwarded.push(args);
      },
    });

    setupBindingFunction(bindings, "leaveChannelCallFromMobile")(
      "general@conference.example.com",
    );

    expect(forwarded).toEqual([
      ["general@conference.example.com"],
    ]);
  });

  test("mobile drawer forwards DM answer rows to the shell handler", async () => {
    const forwarded: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/chat/ChatMobileDrawers.vue", {
      controller: {},
      answerDm: (...args: unknown[]) => {
        forwarded.push(args);
      },
    });

    setupBindingFunction(bindings, "answerDmFromMobile")(
      "bob@example.com",
      "bob@example.com/phone",
      "dm-call-1",
      { audio: true, video: false },
    );

    expect(forwarded).toEqual([
      ["bob@example.com", "bob@example.com/phone", "dm-call-1", { audio: true, video: false }],
    ]);
  });

  test("mobile drawer forwards DM reconnect rows to the shell handler", async () => {
    const forwarded: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/chat/ChatMobileDrawers.vue", {
      controller: {},
      reconnectDm: (...args: unknown[]) => {
        forwarded.push(args);
      },
    });

    setupBindingFunction(bindings, "reconnectDmFromMobile")(
      "bob@example.com",
      { audio: true, video: true },
    );

    expect(forwarded).toEqual([
      ["bob@example.com", { audio: true, video: true }],
    ]);
  });

  test("mobile drawer forwards recovered DM end actions to the shell handler", async () => {
    const forwarded: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/chat/ChatMobileDrawers.vue", {
      controller: {},
      endDm: (...args: unknown[]) => {
        forwarded.push(args);
      },
    });

    setupBindingFunction(bindings, "endDmFromMobile")("bob@example.com");

    expect(forwarded).toEqual([["bob@example.com"]]);
  });

  test("mobile drawer can receive retained hard-refresh group call state from the shell", async () => {
    const bindings = await setupVueComponent("../src/components/chat/ChatMobileDrawers.vue", {
      controller: {
        connectionStore: { client: null, session: null },
        waddles: { mucServiceJid: { value: "conference.example.com" } },
        selfDomain: { value: "example.com" },
      },
      activeChannelCallCount: 1,
      callParticipantCounts: { "general@conference.example.com": 1 },
      callParticipants: { "general@conference.example.com": ["alice"] },
      callMediaByRoom: { "general@conference.example.com": { audio: true, video: true } },
      managedMucDomain: "conference.example.com",
      selfFullJid: "alice@example.com/web",
    });

    expect(setupBindingRefValue(bindings, "visibleActiveChannelCallCount")).toBe(1);
    expect(setupBindingRefValue(bindings, "visibleCallParticipantCounts")).toEqual({
      "general@conference.example.com": 1,
    });
    expect(setupBindingRefValue(bindings, "visibleCallParticipants")).toEqual({
      "general@conference.example.com": ["alice"],
    });
    expect(setupBindingRefValue(bindings, "visibleCallMediaByRoom")).toEqual({
      "general@conference.example.com": { audio: true, video: true },
    });
    expect(setupBindingRefValue(bindings, "visibleManagedMucDomain")).toBe("conference.example.com");
    expect(setupBindingRefValue(bindings, "visibleSelfFullJid")).toBe("alice@example.com/web");
  });

  test("shell surfaces pending retained group-call video media after failed cleanup", async () => {
    markMucCallSessionTerminatePending({
      roomJid: "general@conference.example.com",
      sid: "muc-video-retry",
      selfFullJid: "alice@example.com/web",
      media: { audio: true, video: true },
      now: new Date("2026-05-26T12:00:00.000Z"),
    });

    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/ChatReadyShell.vue", {
        controller: {
          ui: { activeCommunitySurface: { value: null } },
          connectionStore: {
            client: { fullJid: "alice@example.com/web" },
            session: { username: "alice", jid: "alice@example.com" },
          },
          waddles: {
            mucServiceJid: { value: "conference.example.com" },
            sortedSpaces: { value: [] },
            sortedChannels: { value: [] },
            isLoadingStructure: { value: false },
          },
          selfDomain: { value: "example.com" },
          rosterContacts: {
            contacts: { value: [] },
            isLoadingContacts: { value: false },
          },
          messaging: {
            mentionedChannelCounts: { value: {} },
            activeChannels: { value: new Set<string>() },
          },
          dmConversations: { conversations: { value: [] } },
          computedChannelUnreadMap: { value: {} },
          activeRightPanel: { value: null },
          activeThreadStack: { value: [] },
          communityEvents: { rsvp: () => undefined },
          closePinnedPanel: () => undefined,
          activateRightPanel: () => undefined,
          selectDm: () => undefined,
          selectChannel: () => undefined,
          selectChannelByRoomJid: () => undefined,
        },
      })
    );

    expect(setupBindingRefValue(bindings, "retainedMucCallParticipantsStore")).toEqual({
      "general@conference.example.com": ["alice"],
    });
    expect(setupBindingRefValue(bindings, "retainedMucCallMediaStore")).toEqual({
      "general@conference.example.com": { audio: true, video: true },
    });
    expect(setupBindingRefValue<{ callMediaByRoom: Record<string, CallMedia> }>(
      bindings,
      "homeDashboardProps",
    ).callMediaByRoom).toEqual({
      "general@conference.example.com": { audio: true, video: true },
    });
  });

  test("shell keeps live group-call media authoritative over stale pending cleanup media", async () => {
    markMucCallSessionTerminatePending({
      roomJid: "general@conference.example.com",
      sid: "muc-video-retry",
      selfFullJid: "alice@example.com/web",
      media: { audio: true, video: true },
      now: new Date("2026-05-26T12:00:00.000Z"),
    });
    applyMucCallPresence({
      from: "general@conference.example.com/alice",
      muc_jid: "alice@example.com/web",
      muji: { preparing: false, active: true, audio: true, video: false },
    });

    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/ChatReadyShell.vue", {
        controller: {
          ui: { activeCommunitySurface: { value: null } },
          connectionStore: {
            client: { fullJid: "alice@example.com/web" },
            session: { username: "alice", jid: "alice@example.com" },
          },
          waddles: {
            mucServiceJid: { value: "conference.example.com" },
            sortedSpaces: { value: [] },
            sortedChannels: { value: [] },
            isLoadingStructure: { value: false },
          },
          selfDomain: { value: "example.com" },
          rosterContacts: {
            contacts: { value: [] },
            isLoadingContacts: { value: false },
          },
          messaging: {
            mentionedChannelCounts: { value: {} },
            activeChannels: { value: new Set<string>() },
          },
          dmConversations: { conversations: { value: [] } },
          computedChannelUnreadMap: { value: {} },
          activeRightPanel: { value: null },
          activeThreadStack: { value: [] },
          communityEvents: { rsvp: () => undefined },
          closePinnedPanel: () => undefined,
          activateRightPanel: () => undefined,
          selectDm: () => undefined,
          selectChannel: () => undefined,
          selectChannelByRoomJid: () => undefined,
        },
      })
    );

    expect(setupBindingRefValue(bindings, "retainedMucCallMediaStore")).toEqual({
      "general@conference.example.com": { audio: true, video: false },
    });
    expect(setupBindingRefValue<{ callMediaByRoom: Record<string, CallMedia> }>(
      bindings,
      "homeDashboardProps",
    ).callMediaByRoom).toEqual({
      "general@conference.example.com": { audio: true, video: false },
    });
  });

  test("mobile drawer routes community surfaces through the controller", async () => {
    const selected: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/chat/ChatMobileDrawers.vue", {
      controller: {
        openCommunitySurface: (...args: unknown[]) => {
          selected.push(args);
        },
      },
    });

    setupBindingFunction(bindings, "selectCommunitySurface")("feed");

    expect(selected).toEqual([["feed"]]);
  });

  test("mobile drawer preserves room JID when channel call rows open or return", async () => {
    const selected: unknown[][] = [];
    const ui = {
      activeCommunitySurface: { value: "feed" },
    };
    const bindings = await setupVueComponent("../src/components/chat/ChatMobileDrawers.vue", {
      controller: {
        ui,
        selectChannel: (...args: unknown[]) => {
          selected.push(args);
        },
      },
    });

    setupBindingFunction(bindings, "selectChannelFromMobile")(
      "general",
      "general@conference.example.com",
    );

    expect(ui.activeCommunitySurface.value).toBe(null);
    expect(selected).toEqual([
      ["general", { roomJid: "general@conference.example.com" }],
    ]);
  });

  test("mobile drawer opens refreshed group call rows by room JID", async () => {
    const selected: unknown[][] = [];
    const ui = {
      activeCommunitySurface: { value: "feed" },
    };
    const bindings = await setupVueComponent("../src/components/chat/ChatMobileDrawers.vue", {
      controller: {
        ui,
        selectChannel: (...args: unknown[]) => {
          selected.push(args);
        },
        selectChannelByRoomJid: (...args: unknown[]) => {
          selected.push(["room", ...args]);
        },
      },
    });

    setupBindingFunction(bindings, "selectChannelFromMobile")(
      null,
      "general@conference.example.com",
    );

    expect(ui.activeCommunitySurface.value).toBe(null);
    expect(selected).toEqual([
      ["room", "general@conference.example.com"],
    ]);
  });

  test("home dashboard routes refreshed calls through reconnect, open-only, and fallback room actions", async () => {
    const emitted: unknown[][] = [];
    const validDmEntry = buildCallActivityDockEntries({
      channels: [],
      conversations: [{ peerJid: "bob@example.com", peerUsername: "Bob" }],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {},
      dmCallActivities: {
        "bob@example.com": {
          peerJid: "bob@example.com",
          remoteFullJid: "bob@example.com/phone",
          sid: "dm-live",
          media: { audio: true, video: true },
          join: liveKitJoinFor(),
          state: "accepted",
          direction: "incoming",
          updatedAt: "2026-05-25T12:00:00.000Z",
        },
      },
    })[0];
    const staleDmEntry = buildCallActivityDockEntries({
      channels: [],
      conversations: [{ peerJid: "carol@example.com", peerUsername: "Carol" }],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {},
      dmCallActivities: {
        "carol@example.com": {
          peerJid: "carol@example.com",
          remoteFullJid: "carol@example.com/phone",
          sid: "dm-stale",
          media: { audio: true, video: false },
          join: liveKitJoinFor("alice@example.com/other"),
          state: "accepted",
          direction: "incoming",
          updatedAt: "2026-05-25T12:00:00.000Z",
        },
      },
    })[0];
    const fallbackRoomEntry = buildCallActivityDockEntries({
      channels: [],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(["standup@conference.example.com"]),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: { "standup@conference.example.com": 2 },
      dmCallActivities: {},
    })[0];
    const groupDmFallbackRoomEntry = buildCallActivityDockEntries({
      channels: [],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(["group-dm-launch@conference.example.com"]),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: { "group-dm-launch@conference.example.com": 2 },
      callMediaByRoom: { "group-dm-launch@conference.example.com": { audio: true, video: true } },
      dmCallActivities: {},
    })[0];
    if (!validDmEntry || !staleDmEntry || !fallbackRoomEntry || !groupDmFallbackRoomEntry) {
      throw new Error("expected call entries for dashboard routing test");
    }
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/HomeDashboard.vue", {
        spaces: [],
        channels: [],
        contacts: [],
        isLoading: false,
        channelUnreadMap: {},
        activeChannelJids: new Set(["standup@conference.example.com"]),
        managedMucDomain: "conference.example.com",
        callParticipantCounts: {
          "standup@conference.example.com": 2,
          "group-dm-launch@conference.example.com": 2,
        },
        dmConversations: [],
        selfFullJid: "alice@example.com/web",
      }, (...args) => {
        emitted.push(args);
      })
    );

    setupBindingFunction(bindings, "selectCallEntry")(validDmEntry);
    setupBindingFunction(bindings, "selectCallEntry")(staleDmEntry);
    setupBindingFunction(bindings, "selectCallEntry")(fallbackRoomEntry);
    setupBindingFunction(bindings, "selectCallEntry")(groupDmFallbackRoomEntry);

    expect(emitted).toEqual([
      ["reconnectDm", "bob@example.com", { audio: true, video: true }],
      ["selectContact", "carol@example.com"],
      ["joinChannelCall", null, "standup@conference.example.com", { audio: true, video: false }],
      ["joinGroupDmCall", "group-dm-launch@conference.example.com", { audio: true, video: true }],
    ]);
  });

  test("home dashboard uses hydrated group-DM names for active group-DM calls", async () => {
    const html = await renderVueComponent("../src/components/chat/HomeDashboard.vue", {
      spaces: [],
      channels: [],
      contacts: [],
      isLoading: false,
      channelUnreadMap: {},
      groupDms: [
        { id: "gdm-launch", name: "Launch crew", roomJid: "group-dm-launch@conference.example.com" },
      ],
      activeChannelJids: new Set(["group-dm-launch@conference.example.com"]),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: { "group-dm-launch@conference.example.com": 2 },
      callParticipants: { "group-dm-launch@conference.example.com": ["alice", "bob"] },
      callMediaByRoom: { "group-dm-launch@conference.example.com": { audio: true, video: true } },
      dmConversations: [],
      selfFullJid: "alice@example.com/web",
    });

    expect(html).toContain("Launch crew");
    expect(html).not.toContain("group-dm-launch,");
  });

  test("home dashboard leaves retained local group calls without rejoining media", async () => {
    connectionStore.session = {
      username: "alice",
      jid: "alice@example.com",
    } as typeof connectionStore.session;
    connectionStore.client = {
      fullJid: "alice@example.com/web",
    } as unknown as typeof connectionStore.client;
    applyMucCallPresence({
      from: "standup@conference.example.com/alice",
      muc_jid: "alice@example.com/web",
      muji: { preparing: false, active: true },
    });
    const props = {
      spaces: [],
      channels: [],
      contacts: [],
      isLoading: false,
      channelUnreadMap: {},
      activeChannelJids: new Set(["standup@conference.example.com"]),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: { "standup@conference.example.com": 1 },
      callParticipants: { "standup@conference.example.com": ["alice"] },
      dmConversations: [],
      selfFullJid: "alice@example.com/web",
    };
    const html = await renderVueComponent("../src/components/chat/HomeDashboard.vue", props);
    expect(html).toContain('aria-label="Leave standup call"');
    expect(html).toContain("Rejoin");
    expect(html).toContain("Leave call");

    const fallbackRoomEntry = buildCallActivityDockEntries({
      channels: [],
      conversations: [],
      activeChannelId: null,
      activePeerJid: null,
      sidebarMode: "channels",
      activeChannelJids: new Set(["standup@conference.example.com"]),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: { "standup@conference.example.com": 1 },
      callParticipants: { "standup@conference.example.com": ["alice"] },
      dmCallActivities: {},
    })[0];
    if (!fallbackRoomEntry) throw new Error("expected fallback room entry");
    const emitted: unknown[][] = [];
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/HomeDashboard.vue", props, (...args) => {
        emitted.push(args);
      })
    );

    setupBindingFunction(bindings, "endCallEntry")(fallbackRoomEntry);

    expect(emitted).toEqual([
      ["leaveChannelCall", "standup@conference.example.com"],
    ]);
  });

  test("home dashboard channel rows join or open live calls through the call action model", async () => {
    const emitted: unknown[][] = [];
    const channel = { id: "general", name: "General", jid: "general@conference.example.com", spaceId: "team" };
    const props = {
      spaces: [],
      channels: [channel],
      contacts: [],
      isLoading: false,
      channelUnreadMap: {},
      activeChannelJids: new Set<string>(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: { "general@conference.example.com": 2 },
      callParticipants: { "general@conference.example.com": ["alice", "bob"] },
      callMediaByRoom: { "general@conference.example.com": { audio: true, video: true } },
      dmConversations: [],
      selfFullJid: "alice@example.com/web",
    };
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/HomeDashboard.vue", props, (...args) => {
        emitted.push(args);
      })
    );

    setupBindingFunction(bindings, "selectHomeChannel")(channel);
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-live",
      media: { audio: true, video: false },
      join: liveKitJoinFor(),
    });
    const busyBindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/HomeDashboard.vue", props, (...args) => {
        emitted.push(args);
      })
    );
    setupBindingFunction(busyBindings, "selectHomeChannel")(channel);

    expect(emitted).toEqual([
      ["joinChannelCall", "general", "general@conference.example.com", { audio: true, video: true }],
      ["selectChannel", "general", "general@conference.example.com"],
    ]);
  });

  test("shell join-call handler clears community surfaces before selecting a channel", async () => {
    const selected: unknown[][] = [];
    const ui = {
      activeCommunitySurface: { value: "feed" },
    };
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/ChatReadyShell.vue", {
        controller: {
          ui,
          connectionStore: { client: null, session: null },
          waddles: { mucServiceJid: { value: "" } },
          selfDomain: { value: "" },
          rosterContacts: {
            contacts: { value: [] },
            isLoadingContacts: { value: false },
          },
          messaging: {
            mentionedChannelCounts: { value: {} },
            activeChannels: { value: new Set<string>() },
          },
          dmConversations: { conversations: { value: [] } },
          computedChannelUnreadMap: { value: {} },
          activeRightPanel: { value: null },
          activeThreadStack: { value: [] },
          communityEvents: { rsvp: () => undefined },
          closePinnedPanel: () => undefined,
          activateRightPanel: () => undefined,
          selectDm: () => undefined,
          selectChannel: (...args: unknown[]) => {
            selected.push(args);
          },
          selectChannelByRoomJid: (...args: unknown[]) => {
            selected.push(["room", ...args]);
          },
        },
      }),
    );

    setupBindingFunction(bindings, "joinChannelCallFromActivity")(
      "general",
      "general@conference.example.com",
      { audio: true, video: false },
    );

    expect(ui.activeCommunitySurface.value).toBe(null);
    expect(selected).toEqual([
      ["general", { roomJid: "general@conference.example.com" }],
    ]);
  });

  test("shell does not start pre-hydration group-DM calls when selection cannot resolve", async () => {
    const selected: unknown[][] = [];
    let ensureJoinedCalls = 0;
    const ui = {
      activeCommunitySurface: { value: "feed" },
    };
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/ChatReadyShell.vue", {
        controller: {
          ui,
          connectionStore: {
            client: {
              fullJid: "alice@example.com/web",
              ensureJoined: async () => {
                ensureJoinedCalls += 1;
              },
              xmpp: {
                send_muji_session_initiate: async () => {
                  throw new Error("group-DM call should not start before selection resolves");
                },
              },
            },
            session: { username: "alice", jid: "alice@example.com" },
          },
          waddles: { mucServiceJid: { value: "conference.example.com" } },
          selfDomain: { value: "example.com" },
          rosterContacts: {
            contacts: { value: [] },
            isLoadingContacts: { value: false },
          },
          messaging: {
            mentionedChannelCounts: { value: {} },
            activeChannels: { value: new Set<string>() },
          },
          dmConversations: { conversations: { value: [] } },
          computedChannelUnreadMap: { value: {} },
          activeRightPanel: { value: null },
          activeThreadStack: { value: [] },
          communityEvents: { rsvp: () => undefined },
          closePinnedPanel: () => undefined,
          activateRightPanel: () => undefined,
          selectDm: () => undefined,
          selectChannel: () => undefined,
          selectChannelByRoomJid: () => undefined,
          selectGroupDm: async (...args: unknown[]) => {
            selected.push(args);
            return false;
          },
        },
      }),
    );

    setupBindingFunction(bindings, "joinGroupDmCallFromActivity")(
      "group-dm-launch@conference.example.com",
      { audio: true, video: true },
    );
    await Promise.resolve();

    expect(ui.activeCommunitySurface.value).toBe(null);
    expect(selected).toEqual([["group-dm-launch@conference.example.com"]]);
    expect(ensureJoinedCalls).toBe(0);
  });

  test("shell answer selects the DM and proceeds with the hydrated full JID", async () => {
    clearCallState();
    const now = new Date().toISOString();
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-ring",
        media: { audio: true, video: false },
        state: "ringing",
        direction: "incoming",
        updatedAt: now,
      },
    });
    const selected: unknown[][] = [];
    const sendCallProceed = mock(async () => undefined);
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/ChatReadyShell.vue", {
        controller: {
          ui: { activeCommunitySurface: { value: null } },
          connectionStore: {
            client: { fullJid: "alice@example.com/web", xmpp: { send_call_proceed: sendCallProceed } },
            session: null,
          },
          waddles: { mucServiceJid: { value: "" } },
          selfDomain: { value: "" },
          rosterContacts: {
            contacts: { value: [] },
            isLoadingContacts: { value: false },
          },
          messaging: {
            mentionedChannelCounts: { value: {} },
            activeChannels: { value: new Set<string>() },
          },
          dmConversations: { conversations: { value: [] } },
          computedChannelUnreadMap: { value: {} },
          activeRightPanel: { value: null },
          activeThreadStack: { value: [] },
          communityEvents: { rsvp: () => undefined },
          closePinnedPanel: () => undefined,
          activateRightPanel: () => undefined,
          selectDm: (...args: unknown[]) => {
            selected.push(args);
          },
          selectChannel: () => undefined,
          selectChannelByRoomJid: () => undefined,
        },
      }),
    );

    setupBindingFunction(bindings, "answerDmFromActivity")(
      "bob@example.com",
      "bob@example.com/phone",
      "dm-ring",
      { audio: true, video: false },
    );
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(selected).toEqual([["bob@example.com"]]);
    expect(sendCallProceed).toHaveBeenCalledWith("bob@example.com/phone", "dm-ring");
    expect($callState.get()).toMatchObject({
      phase: "incoming",
      from: "bob@example.com/phone",
      sid: "dm-ring",
      accepting: true,
    });
  });

  test("shell reconnect resumes only same-resource archived LiveKit calls", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-resume",
        media: { audio: true, video: true },
        join: liveKitJoinFor(),
        state: "accepted",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });
    const selected: unknown[][] = [];
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/ChatReadyShell.vue", {
        controller: {
          ui: { activeCommunitySurface: { value: null } },
          connectionStore: {
            client: { fullJid: "alice@example.com/web" },
            session: null,
          },
          waddles: { mucServiceJid: { value: "" } },
          selfDomain: { value: "" },
          rosterContacts: {
            contacts: { value: [] },
            isLoadingContacts: { value: false },
          },
          messaging: {
            mentionedChannelCounts: { value: {} },
            activeChannels: { value: new Set<string>() },
          },
          dmConversations: { conversations: { value: [] } },
          computedChannelUnreadMap: { value: {} },
          activeRightPanel: { value: null },
          activeThreadStack: { value: [] },
          communityEvents: { rsvp: () => undefined },
          closePinnedPanel: () => undefined,
          activateRightPanel: () => undefined,
          selectDm: (...args: unknown[]) => {
            selected.push(args);
          },
          selectChannel: () => undefined,
          selectChannelByRoomJid: () => undefined,
        },
      }),
    );

    setupBindingFunction(bindings, "reconnectDmFromDock")(
      "bob@example.com",
      { audio: true, video: true },
    );

    expect(selected).toEqual([["bob@example.com"]]);
    expect($callState.get()).toEqual({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-resume",
      media: { audio: true, video: true },
      join: liveKitJoinFor(),
      initiator: "alice@example.com/web",
    });
  });

  test("shell reconnect uses the reactively published regenerated full JID", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-resume-regenerated",
        media: { audio: true, video: true },
        join: liveKitJoinFor("alice@example.com/recovered"),
        state: "accepted",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });

    const selected: unknown[][] = [];
    const shellConnectionStore = shallowReactive({
      client: { fullJid: "alice@example.com/web" },
      selfFullJid: "alice@example.com/web",
      session: null,
    });
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/ChatReadyShell.vue", {
        controller: {
          ui: { activeCommunitySurface: { value: null } },
          connectionStore: shellConnectionStore,
          waddles: { mucServiceJid: { value: "" } },
          selfDomain: { value: "" },
          rosterContacts: {
            contacts: { value: [] },
            isLoadingContacts: { value: false },
          },
          messaging: {
            mentionedChannelCounts: { value: {} },
            activeChannels: { value: new Set<string>() },
          },
          dmConversations: { conversations: { value: [] } },
          computedChannelUnreadMap: { value: {} },
          activeRightPanel: { value: null },
          activeThreadStack: { value: [] },
          communityEvents: { rsvp: () => undefined },
          closePinnedPanel: () => undefined,
          activateRightPanel: () => undefined,
          selectDm: (...args: unknown[]) => {
            selected.push(args);
          },
          selectChannel: () => undefined,
          selectChannelByRoomJid: () => undefined,
        },
      }),
    );

    shellConnectionStore.selfFullJid = "alice@example.com/recovered";
    await nextTick();
    setupBindingFunction(bindings, "reconnectDmFromDock")(
      "bob@example.com",
      { audio: true, video: true },
    );

    expect(selected).toEqual([["bob@example.com"]]);
    expect($callState.get()).toEqual({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-resume-regenerated",
      media: { audio: true, video: true },
      join: liveKitJoinFor("alice@example.com/recovered"),
      initiator: "alice@example.com/recovered",
    });
  });

  test("shell end action terminates a recovered same-resource DM call", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-recovered-end",
        media: { audio: true, video: true },
        join: liveKitJoinFor(),
        state: "accepted",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });
    const selected: unknown[][] = [];
    const sendCallSessionTerminate = mock(async () => undefined);
    const sendCallFinish = mock(async () => undefined);
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/ChatReadyShell.vue", {
        controller: {
          ui: { activeCommunitySurface: { value: null } },
          connectionStore: {
            client: {
              fullJid: "alice@example.com/web",
              xmpp: {
                send_call_session_terminate: sendCallSessionTerminate,
                send_call_finish: sendCallFinish,
              },
            },
            session: null,
          },
          waddles: { mucServiceJid: { value: "" } },
          selfDomain: { value: "" },
          rosterContacts: {
            contacts: { value: [] },
            isLoadingContacts: { value: false },
          },
          messaging: {
            mentionedChannelCounts: { value: {} },
            activeChannels: { value: new Set<string>() },
          },
          dmConversations: { conversations: { value: [] } },
          computedChannelUnreadMap: { value: {} },
          activeRightPanel: { value: null },
          activeThreadStack: { value: [] },
          communityEvents: { rsvp: () => undefined },
          closePinnedPanel: () => undefined,
          activateRightPanel: () => undefined,
          selectDm: (...args: unknown[]) => {
            selected.push(args);
          },
          selectChannel: () => undefined,
          selectChannelByRoomJid: () => undefined,
        },
      }),
    );

    setupBindingFunction(bindings, "endRecoveredDmFromActivity")("bob@example.com");
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(selected).toEqual([["bob@example.com"]]);
    expect(sendCallSessionTerminate).toHaveBeenCalledWith(
      "bob@example.com/phone",
      "dm-recovered-end",
      "success",
    );
    expect(sendCallFinish).toHaveBeenCalledWith(
      "bob@example.com/phone",
      "dm-recovered-end",
    );
    expect($dmCallActivities.get()["bob@example.com"]).toBeUndefined();
  });

  test("shell end action leaves recovered DM visible when teardown fails", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-recovered-retry",
        media: { audio: true, video: true },
        join: liveKitJoinFor(),
        state: "accepted",
        direction: "incoming",
        updatedAt: new Date().toISOString(),
      },
    });
    const selected: unknown[][] = [];
    const sendCallSessionTerminateWithOutcome = mock(async () => ({ kind: "error" }));
    const sendCallFinish = mock(async () => undefined);
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/ChatReadyShell.vue", {
        controller: {
          ui: { activeCommunitySurface: { value: null } },
          connectionStore: {
            client: {
              fullJid: "alice@example.com/web",
              xmpp: {
                send_call_session_terminate_with_outcome: sendCallSessionTerminateWithOutcome,
                send_call_finish: sendCallFinish,
              },
            },
            session: null,
          },
          waddles: { mucServiceJid: { value: "" } },
          selfDomain: { value: "" },
          rosterContacts: {
            contacts: { value: [] },
            isLoadingContacts: { value: false },
          },
          messaging: {
            mentionedChannelCounts: { value: {} },
            activeChannels: { value: new Set<string>() },
          },
          dmConversations: { conversations: { value: [] } },
          computedChannelUnreadMap: { value: {} },
          activeRightPanel: { value: null },
          activeThreadStack: { value: [] },
          communityEvents: { rsvp: () => undefined },
          closePinnedPanel: () => undefined,
          activateRightPanel: () => undefined,
          selectDm: (...args: unknown[]) => {
            selected.push(args);
          },
          selectChannel: () => undefined,
          selectChannelByRoomJid: () => undefined,
        },
      }),
    );

    setupBindingFunction(bindings, "endRecoveredDmFromActivity")("bob@example.com");
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(selected).toEqual([["bob@example.com"]]);
    expect(sendCallSessionTerminateWithOutcome).toHaveBeenCalledWith(
      "bob@example.com/phone",
      "dm-recovered-retry",
      "success",
    );
    expect(sendCallFinish).not.toHaveBeenCalled();
    expect($dmCallActivities.get()["bob@example.com"]).toMatchObject({
      sid: "dm-recovered-retry",
      state: "accepted",
    });
  });

  test("shell reconnect leaves stale accepted activity open-only instead of sending a new propose", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-stale",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });
    const selected: unknown[][] = [];
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent("../src/components/chat/ChatReadyShell.vue", {
        controller: {
          ui: { activeCommunitySurface: { value: null } },
          connectionStore: {
            client: { fullJid: "alice@example.com/web" },
            session: null,
          },
          waddles: { mucServiceJid: { value: "" } },
          selfDomain: { value: "" },
          rosterContacts: {
            contacts: { value: [] },
            isLoadingContacts: { value: false },
          },
          messaging: {
            mentionedChannelCounts: { value: {} },
            activeChannels: { value: new Set<string>() },
          },
          dmConversations: { conversations: { value: [] } },
          computedChannelUnreadMap: { value: {} },
          activeRightPanel: { value: null },
          activeThreadStack: { value: [] },
          communityEvents: { rsvp: () => undefined },
          closePinnedPanel: () => undefined,
          activateRightPanel: () => undefined,
          selectDm: (...args: unknown[]) => {
            selected.push(args);
          },
          selectChannel: () => undefined,
          selectChannelByRoomJid: () => undefined,
        },
      }),
    );

    setupBindingFunction(bindings, "reconnectDmFromDock")(
      "bob@example.com",
      { audio: true, video: true },
    );

    expect(selected).toEqual([["bob@example.com"]]);
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("renders hydrated DM call controls with reconnect context", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-call-1",
        media: { audio: true, video: true },
        join: liveKitJoinFor(),
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });
    connectionStore.client = { fullJid: "alice@example.com/web" } as unknown as typeof connectionStore.client;

    const html = await renderVueComponent("../src/components/calls/CallButton.vue", {
      peerBareJid: "bob@example.com",
    });

    expect(html).toContain("Video call live");
    expect(html).toContain("Reconnect video call");
    expect(html).not.toContain("Reconnect voice call");
    expect(html).not.toContain("Start voice call");
  });

  test("CallButton reconnects hydrated same-resource activity without sending a fresh propose", async () => {
    const updatedAt = new Date().toISOString();
    const sender = {
      send_call_propose: mock(async () => undefined),
    };
    connectionStore.client = {
      fullJid: "alice@example.com/web",
      xmpp: sender,
    } as unknown as typeof connectionStore.client;
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-call-1",
        media: { audio: true, video: true },
        join: liveKitJoinFor(),
        state: "accepted",
        direction: "incoming",
        updatedAt,
      },
    });
    const bindings = await setupVueComponent("../src/components/calls/CallButton.vue", {
      peerBareJid: "bob@example.com",
    });

    await setupBindingFunction(bindings, "handlePeerCallActivity")();

    expect(sender.send_call_propose).not.toHaveBeenCalled();
    expect($callState.get()).toMatchObject({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-call-1",
      join: liveKitJoinFor(),
    });
  });

  test("CallButton stale accepted activity stays open-only and never sends a new propose", async () => {
    const sender = {
      send_call_propose: mock(async () => undefined),
    };
    connectionStore.client = {
      fullJid: "alice@example.com/web",
      xmpp: sender,
    } as unknown as typeof connectionStore.client;
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-call-stale",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });
    const bindings = await setupVueComponent("../src/components/calls/CallButton.vue", {
      peerBareJid: "bob@example.com",
    });

    await setupBindingFunction(bindings, "handlePeerCallActivity")();

    expect(sender.send_call_propose).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("does not reconnect proceed-only DM call activity with guessed voice media", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-call-unknown-media",
        media: { audio: true, video: false },
        mediaKnown: false,
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/calls/CallButton.vue", {
      peerBareJid: "bob@example.com",
    });

    expect(html).toContain("Call live");
    expect(html).toContain("Open call");
    expect(html).not.toContain("Reconnect voice call");
    expect(html).not.toContain("Reconnect video call");
  });

  test("renders activity-answered incoming calls as connecting in the toast", async () => {
    $callState.set({
      phase: "incoming",
      from: "bob@example.com/phone",
      sid: "dm-call-answering",
      media: { audio: true, video: false },
      accepting: true,
    });

    const html = await renderVueComponent("../src/components/calls/IncomingCallToast.vue", {});

    expect(html).toContain("Connecting");
    expect(html).toContain("disabled");
  });

  test("renders hydrated incoming DM call controls as answer context", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-call-2",
        media: { audio: true, video: false },
        state: "ringing",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/calls/CallButton.vue", {
      peerBareJid: "bob@example.com",
    });

    expect(html).toContain("Incoming voice call");
    expect(html).toContain("Answer voice call");
    expect(html).not.toContain("Reconnect voice call");
  });

  test("renders the group call header button when only ContentArea's room JID is known", async () => {
    const withoutRoom = await renderVueComponent("../src/components/chat/ChatHeader.vue", {
      ...chatHeaderBaseProps(),
      channel: { id: "general", name: "General" },
      callRoomJid: null,
    });
    const withRoom = await renderVueComponent("../src/components/chat/ChatHeader.vue", {
      ...chatHeaderBaseProps(),
      channel: { id: "general", name: "General" },
      callRoomJid: "general@custom-muc.example.test",
    });

    expect(withoutRoom).not.toContain('data-vue-stub="true"');
    expect(withRoom).toContain('data-vue-stub="true"');
  });

  test("suppresses the header DM activity status when the conversation banner owns it", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        remoteFullJid: "bob@example.com/phone",
        sid: "dm-live",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });
    const props = {
      ...chatHeaderBaseProps(),
      dmPeer: { peerJid: "bob@example.com", peerUsername: "Bob", presenceShow: "available" },
    };

    const visible = await renderVueComponent("../src/components/chat/ChatHeader.vue", props);
    const suppressed = await renderVueComponent("../src/components/chat/ChatHeader.vue", {
      ...props,
      showDmCallActivityControls: false,
    });

    expect(visible).toContain("Video call active");
    expect(suppressed).not.toContain("Video call active");
    expect(suppressed).toContain("online");
  });
});

function chatHeaderBaseProps(): Record<string, unknown> {
  return {
    waddle: { id: "team", name: "Team" },
    channel: null,
    dmPeer: null,
    isForumChannel: false,
    canManageChannels: false,
    memberCount: 0,
    memberState: "ready",
    members: [],
    connectionNotice: null,
    connectionStatusClasses: null,
    connectionStatusIcon: { name: "StatusIcon", render: () => null },
    xmppClient: null,
    notifySettings: {},
    showSearch: false,
    "onUpdate:showSearch": () => undefined,
    showPinnedPanel: false,
    "onUpdate:showPinnedPanel": () => undefined,
  };
}

function contentAreaBaseProps(): Record<string, unknown> {
  return {
    draft: "",
    forumTitle: "",
    pinnedPanelOpen: false,
    waddle: { id: "team", name: "Team" },
    channel: { id: "general", name: "General", jid: "general@conference.example.com", spaceId: "team" },
    roomJid: null,
    dmPeer: { peerJid: "bob@example.com", peerUsername: "Bob", presenceShow: "available" },
    sidebarMode: "channels",
    messages: [],
    firstUnseenId: null,
    xmppStatus: { state: "online" },
    actionError: "",
    errorActionLabel: null,
    updateAvailable: false,
    isApplyingUpdate: false,
    isLoadingMessages: false,
    isLoadingOlderMessages: false,
    hasOlderMessages: false,
    isSending: false,
    canManageChannels: false,
    memberCount: 0,
    memberState: "ready",
    typingUsers: [],
    currentUser: "alice",
    currentUserJid: "alice@example.com",
    selfFullJid: "alice@example.com/web",
    selfDomain: "example.com",
    avatarUrlByAuthor: {},
    authorJidByNick: {},
    mentionCandidates: [],
    roomHats: {},
    roomAuthority: {},
    roomPresence: {},
    roomLastSeen: {},
    slowModeCooldown: 0,
    searchResults: [],
    isSearching: false,
    uploadProgress: { uploading: false, progress: 0, filename: "" },
    threadIndex: new Map(),
    xmppClient: null,
    notifySettings: {},
    reactionMode: null,
  };
}

function topicsPanelBaseProps(): Record<string, unknown> {
  return {
    waddle: null,
    spaces: [],
    channels: [],
    activeChannelId: null,
    canManageChannels: false,
    canManageCommunity: false,
    isLoading: false,
    memberCount: 0,
    memberState: "ready",
    activeChannelJids: new Set<string>(),
    collapsedGroupIds: new Set<string>(),
  };
}

async function emittedTopicChannelRowEvents(callState?: CallState): Promise<unknown[][]> {
  clearCallState();
  if (callState) $callState.set(callState);
  const emitted: unknown[][] = [];
  const channel = { id: "general", name: "General", jid: "general@conference.example.com" };
  const bindings = await setupVueComponent("../src/components/chat/TopicsPanel.vue", {
    ...topicsPanelBaseProps(),
    channels: [channel],
    callParticipantCounts: {
      "general@conference.example.com": 2,
    },
    managedMucDomain: "conference.example.com",
  }, (...args) => {
    emitted.push(args);
  });

  setupBindingFunction(bindings, "selectChannelRow")(channel);
  return emitted;
}

async function emittedRefreshedGroupCallRowEvents(callState?: CallState): Promise<unknown[][]> {
  clearCallState();
  if (callState) $callState.set(callState);
  const emitted: unknown[][] = [];
  const bindings = await setupVueComponent("../src/components/chat/TopicsPanel.vue", {
    ...topicsPanelBaseProps(),
    callParticipantCounts: {
      "general@conference.example.com": 2,
    },
    managedMucDomain: "conference.example.com",
  }, (...args) => {
    emitted.push(args);
  });

  setupBindingFunction(bindings, "selectRefreshedGroupCallRow")({
    key: "group-call:general@conference.example.com",
    roomJid: "general@conference.example.com",
    title: "general",
    participantCount: 2,
    media: { audio: true, video: false },
  });
  return emitted;
}

async function renderCallActivityDock(props: Record<string, unknown>) {
  return renderVueComponent("../src/components/calls/CallActivityDock.vue", props);
}

async function renderVueComponent(path: string, props: Record<string, unknown>) {
  const component = await loadVueComponent(path);
  return renderToString(createSSRApp({ render: () => h(component, props) }));
}

async function setupVueComponent(
  path: string,
  props: Record<string, unknown>,
  emit: (...args: unknown[]) => void = () => undefined,
): Promise<Record<string, unknown>> {
  const component = await loadVueComponent(path, { inlineTemplate: false }) as {
    setup?: (
      props: Record<string, unknown>,
      context: {
        emit: (...args: unknown[]) => void;
        expose: () => void;
        attrs: Record<string, unknown>;
        slots: Record<string, unknown>;
      },
    ) => Record<string, unknown>;
  };
  if (!component.setup) throw new Error(`${path} has no setup function`);
  return component.setup(props, {
    emit,
    expose: () => undefined,
    attrs: {},
    slots: {},
  });
}

function setupBindingFunction(
  bindings: Record<string, unknown>,
  key: string,
): (...args: unknown[]) => unknown {
  const binding = bindings[key];
  if (typeof binding !== "function") {
    throw new Error(`Expected setup binding ${key} to be a function`);
  }
  return binding as (...args: unknown[]) => unknown;
}

function setupBindingRefValue<T = unknown>(
  bindings: Record<string, unknown>,
  key: string,
): T {
  const binding = bindings[key];
  if (!binding || typeof binding !== "object" || !("value" in binding)) {
    throw new Error(`Expected setup binding ${key} to be a ref`);
  }
  return (binding as { value: T }).value;
}

async function suppressVueLifecycleSetupWarnings<T>(fn: () => Promise<T>): Promise<T> {
  const originalWarn = console.warn;
  console.warn = (...args: unknown[]) => {
    const message = String(args[0] ?? "");
    if (
      message.includes("onMounted is called when there is no active component instance") ||
      message.includes("onBeforeUnmount is called when there is no active component instance") ||
      message.includes("onUnmounted is called when there is no active component instance")
    ) {
      return;
    }
    originalWarn(...args);
  };
  try {
    return await fn();
  } finally {
    console.warn = originalWarn;
  }
}

async function loadVueComponent(path: string, options: { inlineTemplate?: boolean } = {}) {
  const filename = new URL(path, import.meta.url);
  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, {
    id: filename.pathname,
    inlineTemplate: options.inlineTemplate ?? true,
  });

  const tempDir = mkdtempSync(join(tmpdir(), "waddle-vue-component-"));
  try {
    const compiled = [
      rewriteImports(script.content.replace("export default", "const __sfc__ ="), filename),
      "export default __sfc__;",
    ].join("\n");

    const js = ts.transpileModule(compiled, {
      compilerOptions: {
        module: ts.ModuleKind.ESNext,
        target: ts.ScriptTarget.ES2022,
        verbatimModuleSyntax: false,
      },
    }).outputText;
    const modulePath = join(tempDir, "Component.mjs");
    writeFileSync(modulePath, js);
    const component = await import(pathToFileURL(modulePath).href);
    return component.default;
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

function rewriteImports(code: string, importer: URL): string {
  return code.replace(/from\s+["']([^"']+)["']/g, (_match, specifier: string) =>
    `from ${JSON.stringify(resolveModuleSpecifier(specifier, importer))}`);
}

function resolveModuleSpecifier(specifier: string, importer: URL): string {
  if (specifier.startsWith("@/")) {
    return moduleUrlForPath(resolveSourcePath(new URL(`../src/${specifier.slice(2)}`, import.meta.url).pathname));
  }
  if (specifier.startsWith(".")) {
    return moduleUrlForPath(resolveSourcePath(new URL(specifier, importer).pathname));
  }
  return import.meta.resolve(specifier);
}

function resolveSourcePath(basePath: string): string {
  const candidates = [
    basePath,
    `${basePath}.ts`,
    `${basePath}.tsx`,
    `${basePath}.js`,
    `${basePath}.mjs`,
    `${basePath}.vue`,
    `${basePath}.json`,
    `${basePath}/index.ts`,
  ];
  const resolved = candidates.find((candidate) => existsSync(candidate));
  if (!resolved) throw new Error(`Unable to resolve test SFC import: ${basePath}`);
  return resolved;
}

function moduleUrlForPath(resolvedPath: string): string {
  if (!resolvedPath.endsWith(".vue")) return pathToFileURL(resolvedPath).href;
  if (resolvedPath.endsWith("/components/calls/ConversationCallBanner.vue")) {
    return new URL("./helpers/conversation-call-banner-stub.ts", import.meta.url).href;
  }
  return new URL("./helpers/vue-sfc-stub.ts", import.meta.url).href;
}
