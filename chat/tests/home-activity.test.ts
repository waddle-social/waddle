import { afterEach, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { parse, compileScript } from "vue/compiler-sfc";
import { createSSRApp, h } from "vue";
import { renderToString } from "vue/server-renderer";
import ts from "typescript";
import { formatTimelineStamp } from "../src/channels/timeline";
import {
  channelActivityState,
  channelActivityPreview,
  channelHomeLabel,
  channelUnreadBadgeCount,
  compareChannelActivityPriority,
  dmHomeLabel,
  dmPresenceLabel,
  dmPreviewText,
  homeActivityLabel,
  spaceHomeLabel,
  summarizeHomeActivity,
} from "../src/home/activity";
import { buildHomeChannelUnreadMap, buildHomeDashboardProps } from "../src/home/dashboard-props";
import { $callState, clearCallState } from "../src/lib/calls/call-store";
import type { ChannelSummary } from "../src/lib/chat-types";

const channels: ChannelSummary[] = [
  { id: "general", name: "General", jid: "general@conference.example.com", spaceId: "team" },
  { id: "planning", name: "Planning", jid: "planning@conference.example.com", spaceId: "team" },
  { id: "quiet", name: "Quiet", jid: "quiet@conference.example.com", spaceId: "team" },
];

afterEach(() => {
  clearCallState();
});

function liveKitToken(exp = 4_102_444_800): string {
  return [
    base64Url(JSON.stringify({ alg: "none", typ: "JWT" })),
    base64Url(JSON.stringify({ exp })),
    "sig",
  ].join(".");
}

function base64Url(value: string): string {
  return btoa(value).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

describe("home activity summaries", () => {
  test("summarizes unread, mentions, and live room activity for spaces", () => {
    const summary = summarizeHomeActivity(
      channels,
      {
        general: { unread: 4, mentions: 0 },
        planning: {
          unread: 2,
          mentions: 1,
          threadUnread: 3,
          preview: "Thread reply preview",
          lastUpdated: 1_767_000_000,
        },
      },
      new Set(["quiet@conference.example.com"]),
    );

    expect(summary).toEqual({
      channelCount: 3,
      unread: 6,
      mentions: 1,
      threadUnread: 3,
      preview: "Thread reply preview",
      lastUpdated: 1_767_000_000,
      hasActivity: true,
    });
    expect(spaceHomeLabel("Team", summary, "Planning")).toBe("Team, opens Planning, 3 channels, 1 mention, 6 unread, 3 thread replies, live activity, preview: Thread reply preview");
    expect(channelActivityPreview(summary)).toBe("Thread reply preview");
    expect(channelUnreadBadgeCount({ unread: 2, threadUnread: 3 })).toBe(2);
    expect([...channels]
      .sort((a, b) => compareChannelActivityPriority(
        channelActivityState(a, {
          general: { unread: 4, mentions: 0 },
          planning: { unread: 2, mentions: 1, threadUnread: 3 },
        }, new Set(["quiet@conference.example.com"])),
        channelActivityState(b, {
          general: { unread: 4, mentions: 0 },
          planning: { unread: 2, mentions: 1, threadUnread: 3 },
        }, new Set(["quiet@conference.example.com"])),
      ))
      .map((channel) => channel.id)).toEqual(["planning", "general", "quiet"]);
  });

  test("labels per-channel unread state for Home navigation rows", () => {
    const activity = channelActivityState(
      channels[0],
      { general: { unread: 2, mentions: 0 } },
      new Set<string>(),
    );

    expect(homeActivityLabel(activity)).toBe("2 unread");
    expect(channelHomeLabel(channels[0], activity)).toBe("General, channel, 2 unread");
  });

  test("falls back to active signal when unread state is not available", () => {
    const activity = channelActivityState(
      { id: "general", jid: "general@conference.example.com" },
      undefined,
      new Set(["general@conference.example.com"]),
    );

    expect(activity).toEqual({
      unread: 0,
      mentions: 0,
      threadUnread: 0,
      hasActivity: true,
    });
    expect(homeActivityLabel(activity)).toBe("live activity");
  });

  test("matches active channels by exact room JID only", () => {
    expect(channelActivityState(
      { id: "chat", jid: "chat@conference.example.com" },
      undefined,
      new Set(["chat@conference.example.com/desktop"]),
    ).hasActivity).toBe(true);
    expect(channelActivityState(
      { id: "chat" },
      undefined,
      new Set(["chat@conference.example.com"]),
    ).hasActivity).toBe(false);
    expect(channelActivityState(
      { id: "chat", jid: "chat@conference.example.com" },
      undefined,
      new Set(["dispatch@conference.example.com", "chatty@conference.example.com"]),
    ).hasActivity).toBe(false);
  });

  test("labels direct message activity with unread and preview state", () => {
    const label = dmHomeLabel({
      peerJid: "bob@example.com",
      peerUsername: "bob",
      lastMessageBody: "Can you review the plan?",
      unreadCount: 3,
      presenceShow: "away",
    });

    expect(label).toBe("bob (bob@example.com), direct message, away, 3 unread, last message: Can you review the plan?");
    expect(dmPresenceLabel("xa")).toBe("extended away");
    expect(dmPreviewText("x".repeat(70))).toBe(`${"x".repeat(64)}...`);
  });

  test("reports no unread activity for quiet channels", () => {
    const activity = channelActivityState(channels[2], {}, new Set<string>());

    expect(activity).toEqual({
      unread: 0,
      mentions: 0,
      threadUnread: 0,
      hasActivity: false,
    });
    expect(homeActivityLabel(activity)).toBe("no unread activity");
  });

  test("overlays live mention activity onto channel unread state by room JID", () => {
    const map = buildHomeChannelUnreadMap(
      channels,
      {
        general: {
          unread: 0,
          mentions: 0,
          threadUnread: 2,
          preview: "Recent update",
          lastUpdated: 1_767_000_000,
        },
      },
      { "general@conference.example.com": 2, "stale@conference.example.com": 4 },
    );

    expect(map.general).toEqual({
      unread: 0,
      mentions: 2,
      threadUnread: 2,
      preview: "Recent update",
      lastUpdated: 1_767_000_000,
    });
    expect(map.planning.mentions).toBe(0);
  });
});

async function loadHomeDashboardComponent() {
  const filename = new URL("../src/components/chat/HomeDashboard.vue", import.meta.url);
  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, { id: "home-dashboard-test", inlineTemplate: true });

  const tempDir = mkdtempSync(join(tmpdir(), "waddle-home-dashboard-"));
  try {
    const compiled = [
      rewriteImports(script.content.replace("export default", "const __sfc__ ="), filename, tempDir),
      "export default __sfc__;",
    ].join("\n");

    const js = ts.transpileModule(compiled, {
      compilerOptions: {
        module: ts.ModuleKind.ESNext,
        target: ts.ScriptTarget.ES2022,
        verbatimModuleSyntax: false,
      },
    }).outputText;
    const modulePath = join(tempDir, "HomeDashboard.mjs");
    writeFileSync(modulePath, js);
    const component = await import(pathToFileURL(modulePath).href);
    return component.default;
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

function rewriteImports(code: string, importer: URL, tempDir: string): string {
  return code.replace(/from\s+["']([^"']+)["']/g, (_match, specifier: string) =>
    `from ${JSON.stringify(resolveModuleSpecifier(specifier, importer, tempDir))}`);
}

function resolveModuleSpecifier(specifier: string, importer: URL, tempDir: string): string {
  if (specifier.startsWith("@/")) {
    return moduleUrlForPath(resolveSourcePath(new URL(`../src/${specifier.slice(2)}`, import.meta.url).pathname), specifier, tempDir);
  }
  if (specifier.startsWith(".")) {
    return moduleUrlForPath(resolveSourcePath(new URL(specifier, importer).pathname), specifier, tempDir);
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

function moduleUrlForPath(resolvedPath: string, specifier: string, tempDir: string): string {
  if (!resolvedPath.endsWith(".vue")) return pathToFileURL(resolvedPath).href;
  const stubPath = join(tempDir, `${specifier.replace(/[^a-z0-9]/gi, "_")}.mjs`);
  writeFileSync(stubPath, [
    `import { h } from ${JSON.stringify(import.meta.resolve("vue"))};`,
    `export default { name: ${JSON.stringify(`${specifier}Stub`)}, setup(_, { slots }) { return () => h("span", { "data-vue-stub": ${JSON.stringify(specifier)} }, slots.default?.()); } };`,
  ].join("\n"));
  return pathToFileURL(stubPath).href;
}

async function renderHomeDashboard(props: Record<string, unknown>) {
  const component = await loadHomeDashboardComponent();
  return renderToString(createSSRApp({ render: () => h(component, props) }));
}

describe("HomeDashboard activity rendering", () => {
  test("renders channel, direct-message, and empty-space activity states", async () => {
    const html = await renderHomeDashboard({
      spaces: [
        { id: "team", name: "Team" },
        { id: "empty", name: "Empty" },
      ],
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com", spaceId: "team" },
        { id: "forum", name: "Forum", jid: "forum@conference.example.com", spaceId: "team", channel_type: "forum" },
        { id: "random", name: "Random", jid: "random@conference.example.com" },
      ],
      contacts: [],
      isLoading: false,
      channelUnreadMap: {
        general: {
          unread: 4,
          mentions: 0,
          preview: "New launch plan",
          lastUpdated: 1_767_000_000,
        },
        forum: {
          unread: 0,
          mentions: 1,
          threadUnread: 3,
          preview: "Forum thread reply",
          lastUpdated: 1_767_000_060,
        },
      },
      activeChannelJids: new Set(["random@conference.example.com"]),
      dmConversations: [
        {
          peerJid: "bob@example.com",
          peerUsername: "bob",
          peerAvatarUrl: "https://example.com/bob.png",
          lastMessageBody: "Can you review the plan?",
          lastMessageAt: "2026-05-08T13:00:00Z",
          unreadCount: 2,
          presenceShow: "available",
        },
      ],
    });
    const generalLabel = `General, channel, 4 unread, preview: New launch plan, last updated: ${formatTimelineStamp(new Date(1_767_000_000 * 1000).toISOString())}`;
    const forumLabel = `Forum, channel, 1 mention, 3 thread replies, preview: Forum thread reply, last updated: ${formatTimelineStamp(new Date(1_767_000_060 * 1000).toISOString())}`;
    const bobLabel = `bob (bob@example.com), direct message, available, 2 unread, last message: Can you review the plan?, last updated: ${formatTimelineStamp("2026-05-08T13:00:00Z")}`;

    expect(html).toContain(`aria-label="${generalLabel}"`);
    expect(html).toContain("New launch plan");
    expect(html).toContain('aria-label="Team, opens Forum, 2 channels, 1 mention, 4 unread, 3 thread replies, preview: Forum thread reply"');
    expect(html).toContain("2 channels · Opens Forum · Forum thread reply");
    expect(html).toContain(`aria-label="${forumLabel}"`);
    expect(html).toContain("Forum thread reply");
    expect(buttonForLabel(html, forumLabel)).toContain(">@1</span>");
    expect(buttonForLabel(html, forumLabel)).toContain(">3 replies</span>");
    expect(html).toContain('aria-label="Random, channel, live activity"');
    expect(buttonForLabel(html, generalLabel)).toContain(">4</span>");
    expect(html).toContain("Direct messages");
    expect(html).toContain('data-vue-stub="@/components/ui/AppAvatar.vue"');
    expect(html).toContain(`aria-label="${bobLabel}"`);
    expect(html).toContain("bob@example.com · Can you review the plan?");
    expect(buttonForLabel(html, bobLabel)).toContain(">available</span>");
    expect(html).toContain('aria-label="Empty, 0 channels, no unread activity"');
    expect(buttonForLabel(html, "Empty, 0 channels, no unread activity")).toContain("disabled");
  });

  test("keeps empty spaces out of the channel overview empty state", async () => {
    const html = await renderHomeDashboard({
      spaces: [{ id: "empty", name: "Empty" }],
      channels: [],
      contacts: [],
      isLoading: false,
      channelUnreadMap: {},
      activeChannelJids: new Set<string>(),
      dmConversations: [],
    });

    expect(html).toContain("No channels discovered.");
    expect(html).toContain("No direct messages yet.");
    expect(html).toContain('aria-label="Empty, 0 channels, no unread activity"');
    expect(buttonForLabel(html, "Empty, 0 channels, no unread activity")).toContain("disabled");
    expect(buttonForLabel(html, "Empty, 0 channels, no unread activity")).not.toContain("opacity-75");
  });

  test("renders refresh-discovered active calls on the home dashboard", async () => {
    const updated = formatTimelineStamp("2026-05-26T00:00:00.000Z");
    const html = await renderHomeDashboard({
      spaces: [{ id: "team", name: "Team" }],
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com", spaceId: "team" },
      ],
      contacts: [],
      isLoading: false,
      channelUnreadMap: {},
      activeChannelJids: new Set<string>(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {
        "general@conference.example.com": 2,
      },
      callParticipants: {
        "general@conference.example.com": ["alice", "bob"],
      },
      dmConversations: [
        {
          peerJid: "bob@example.com",
          peerUsername: "Bob",
          unreadCount: 0,
          presenceShow: "available",
        },
      ],
      dmCallActivities: {
        "bob@example.com": {
          peerJid: "bob@example.com",
          remoteFullJid: "bob@example.com/phone",
          sid: "dm-call-1",
          media: { audio: true, video: true },
          join: {
            url: "wss://livekit.example.test",
            room: "dm-call-1",
            identity: "alice@example.com/web",
            token: liveKitToken(),
          },
          state: "accepted",
          direction: "incoming",
          updatedAt: "2026-05-26T00:00:00.000Z",
        },
      },
      selfFullJid: "alice@example.com/web",
    });

    expect(html).toContain("Active calls");
    expect(html).toContain("<strong>2</strong> active calls");
    expect(html).toContain("2 live conversations");
    expect(html).toContain("Live now");
    expect(html).toContain("2 people connected in this channel: alice, bob.");
    expect(html).toContain("alice, bob");
    expect(html).toContain("The video call is still live.");
    expect(html).toContain('aria-label="Join General, Group call, 2 people, Live now, 2 people connected in this channel: alice, bob., Group call"');
    expect(html).toContain(`aria-label="Reconnect Bob, Video call, Live, Live now, The video call is still live., Live video call · Updated ${updated}"`);
    expect(buttonForLabel(html, "Join General, Group call, 2 people, Live now, 2 people connected in this channel: alice, bob., Group call")).toContain("Join");
    expect(buttonForLabel(html, "General, channel, no unread activity, active call with 2 people")).toContain("Active call");
    expect(buttonForLabel(html, `Reconnect Bob, Video call, Live, Live now, The video call is still live., Live video call · Updated ${updated}`)).toContain("Live");
    expect(html).not.toContain("Bob (bob@example.com), direct message, available, no unread activity, Video call live");
  });

  test("labels non-resumable active direct calls on the home dashboard", async () => {
    const updated = formatTimelineStamp("2026-05-26T00:00:00.000Z");
    const html = await renderHomeDashboard({
      spaces: [],
      channels: [],
      contacts: [],
      isLoading: false,
      channelUnreadMap: {},
      activeChannelJids: new Set<string>(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {},
      dmConversations: [
        {
          peerJid: "bob@example.com",
          peerUsername: "Bob",
          unreadCount: 0,
          presenceShow: "available",
        },
      ],
      dmCallActivities: {
        "bob@example.com": {
          peerJid: "bob@example.com",
          remoteFullJid: "bob@example.com/phone",
          sid: "dm-call-other-device",
          media: { audio: true, video: false },
          join: {
            url: "wss://livekit.example.test",
            room: "dm-call-other-device",
            identity: "alice@example.com/phone",
            token: liveKitToken(),
          },
          state: "accepted",
          direction: "incoming",
          updatedAt: "2026-05-26T00:00:00.000Z",
        },
      },
      selfFullJid: "alice@example.com/web",
    });

    expect(html).toContain("Other device");
    expect(html).toContain("This call is live on another browser or device.");
    expect(html).toContain("Voice call · Other device");
    expect(html).toContain(`aria-label="Open Bob, Voice call, Live, Other device, This call is live on another browser or device., Voice call · Other device · Updated ${updated}"`);
    expect(html).not.toContain("Reconnect Bob");
  });

  test("labels expired active direct calls on the home dashboard", async () => {
    const updated = formatTimelineStamp("2026-05-26T00:00:00.000Z");
    const html = await renderHomeDashboard({
      spaces: [],
      channels: [],
      contacts: [],
      isLoading: false,
      channelUnreadMap: {},
      activeChannelJids: new Set<string>(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {},
      dmConversations: [
        {
          peerJid: "bob@example.com",
          peerUsername: "Bob",
          unreadCount: 0,
          presenceShow: "available",
        },
      ],
      dmCallActivities: {
        "bob@example.com": {
          peerJid: "bob@example.com",
          remoteFullJid: "bob@example.com/phone",
          sid: "dm-call-expired",
          media: { audio: true, video: true },
          join: {
            url: "wss://livekit.example.test",
            room: "dm-call-expired",
            identity: "alice@example.com/web",
            token: liveKitToken(Math.floor(Date.now() / 1000) + 10),
          },
          state: "accepted",
          direction: "incoming",
          updatedAt: "2026-05-26T00:00:00.000Z",
        },
      },
      selfFullJid: "alice@example.com/web",
    });

    expect(html).toContain("Expired");
    expect(html).toContain("The saved reconnect details expired.");
    expect(html).toContain("Video call · Expired");
    expect(html).toContain(`aria-label="Open Bob, Video call, Live, Expired, The saved reconnect details expired., Video call · Expired · Updated ${updated}"`);
    expect(html).not.toContain("Reconnect Bob");
  });

  test("counts the local current call before discovery echoes activity on the home dashboard", async () => {
    $callState.set({
      phase: "active",
      kind: "dm",
      peer: "bob@example.com/phone",
      sid: "dm-current",
      media: { audio: true, video: true },
      join: {
        url: "wss://livekit.example.test",
        room: "dm-current",
        identity: "alice@example.com/web",
        token: liveKitToken(),
      },
    });

    const html = await renderHomeDashboard({
      spaces: [],
      channels: [],
      contacts: [],
      isLoading: false,
      channelUnreadMap: {},
      activeChannelJids: new Set<string>(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {},
      dmConversations: [
        {
          peerJid: "bob@example.com",
          peerUsername: "Bob",
          unreadCount: 0,
          presenceShow: "available",
        },
      ],
      dmCallActivities: {},
      selfFullJid: "alice@example.com/web",
    });

    expect(html).toContain("<strong>1</strong> active call");
    expect(html).toContain("1 live conversation");
    expect(html).toContain("Bob");
    expect(html).toContain("The video call is still live.");
    expect(html).toContain("Return");
    expect(html).not.toContain("Bob (bob@example.com), direct message");
    expect(html).not.toContain("No active calls.");
  });

  test("counts and renders the local current group call before Muji discovery echoes activity", async () => {
    $callState.set({
      phase: "muc-pending",
      kind: "muc",
      peer: "general@conference.example.com",
      sid: "muc-current",
      media: { audio: true, video: false },
      selfNick: "alice",
      attemptId: "attempt-1",
    });

    const html = await renderHomeDashboard({
      spaces: [{ id: "team", name: "Team" }],
      channels: [
        { id: "general", name: "General", jid: "general@conference.example.com", spaceId: "team" },
      ],
      contacts: [],
      isLoading: false,
      channelUnreadMap: {},
      activeChannelJids: new Set<string>(),
      managedMucDomain: "conference.example.com",
      callParticipantCounts: {},
      dmConversations: [],
    });

    expect(html).toContain("<strong>1</strong> active call");
    expect(html).toContain("1 live conversation");
    expect(html).toContain("General");
    expect(html).toContain("1 person connected in this channel: alice.");
    expect(html).toContain('aria-label="Return General, Group call, 1 person, Live now, 1 person connected in this channel: alice., Group call"');
    expect(html).not.toContain("No active calls.");
  });

  test("pluralizes single thread reply badges", async () => {
    const html = await renderHomeDashboard({
      spaces: [{ id: "team", name: "Team" }],
      channels: [
        { id: "forum", name: "Forum", jid: "forum@conference.example.com", spaceId: "team", channel_type: "forum" },
      ],
      contacts: [],
      isLoading: false,
      channelUnreadMap: {
        forum: {
          unread: 0,
          mentions: 0,
          threadUnread: 1,
          preview: "One reply",
        },
      },
    });

    expect(html).toContain(">1 reply</span>");
    expect(html).not.toContain(">1 replies</span>");
  });

  test("maps existing inbox and DM state into Home props", () => {
    const props = buildHomeDashboardProps({
      spaces: [{ id: "team", name: "Team" }],
      channels,
      contacts: [{ jid: "bob@example.com", username: "bob" }],
      isLoading: false,
      channelUnreadMap: {
        general: {
          unread: 4,
          mentions: 0,
          threadUnread: 2,
          preview: "from inbox",
          lastUpdated: 1_767_000_000,
        },
      },
      mentionedRoomJids: { "general@conference.example.com": 1 },
      activeChannelJids: new Set(["general@conference.example.com"]),
      dmConversations: [{
        peerJid: "bob@example.com",
        peerUsername: "bob",
        unreadCount: 2,
        lastMessageBody: "Can you review the plan?",
      }],
      callParticipantCounts: { "general@conference.example.com": 2 },
      callParticipants: { "general@conference.example.com": ["alice", "bob"] },
      dmCallActivities: {
        "bob@example.com": {
          peerJid: "bob@example.com",
          sid: "dm-call-1",
          media: { audio: true, video: false },
          state: "accepted",
          direction: "incoming",
          updatedAt: "2026-05-26T00:00:00.000Z",
        },
      },
      managedMucDomain: "conference.example.com",
      selfFullJid: "alice@example.com/web",
    });

    expect(props.channelUnreadMap?.general).toEqual({
      unread: 4,
      mentions: 1,
      threadUnread: 2,
      preview: "from inbox",
      lastUpdated: 1_767_000_000,
    });
    expect(props.activeChannelJids?.has("general@conference.example.com")).toBe(true);
    expect(props.dmConversations?.[0]?.unreadCount).toBe(2);
    expect(props.callParticipantCounts).toEqual({ "general@conference.example.com": 2 });
    expect(props.callParticipants).toEqual({ "general@conference.example.com": ["alice", "bob"] });
    expect(props.dmCallActivities?.["bob@example.com"]?.state).toBe("accepted");
    expect(props.managedMucDomain).toBe("conference.example.com");
    expect(props.selfFullJid).toBe("alice@example.com/web");
  });
});

function buttonForLabel(html: string, label: string): string {
  const escaped = label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = html.match(new RegExp(`<button\\b(?=[^>]*aria-label="${escaped}")[\\s\\S]*?</button>`));
  if (!match) throw new Error(`button not found for label: ${label}`);
  return match[0];
}
