import { describe, expect, test } from "bun:test";
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
import type { ChannelSummary } from "../src/lib/chat-types";

const channels: ChannelSummary[] = [
  { id: "general", name: "General", jid: "general@conference.example.com", spaceId: "team" },
  { id: "planning", name: "Planning", jid: "planning@conference.example.com", spaceId: "team" },
  { id: "quiet", name: "Quiet", jid: "quiet@conference.example.com", spaceId: "team" },
];

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
  });
});

function buttonForLabel(html: string, label: string): string {
  const escaped = label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = html.match(new RegExp(`<button\\b(?=[^>]*aria-label="${escaped}")[\\s\\S]*?</button>`));
  if (!match) throw new Error(`button not found for label: ${label}`);
  return match[0];
}
