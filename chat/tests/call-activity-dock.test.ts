import { afterEach, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createSSRApp, h } from "vue";
import { parse, compileScript } from "vue/compiler-sfc";
import { renderToString } from "vue/server-renderer";
import ts from "typescript";
import {
  buildCallActivityDockEntries,
  callActivityDockSelection,
} from "../src/lib/calls/call-activity-dock";
import { clearMucCallParticipants, $mucCallParticipants } from "../src/lib/calls/muc-call-presence";
import { clearDmCallActivities, $dmCallActivities } from "../src/lib/calls/dm-call-activity";
import type { DmCallActivity } from "../src/lib/calls/dm-call-activity";

afterEach(() => {
  clearMucCallParticipants();
  clearDmCallActivities();
});

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
        isKnownChannel: true,
        isActive: true,
      },
      {
        kind: "dm",
        key: "dm:bob@example.com:dm-call-1",
        peerJid: "bob@example.com",
        title: "Bob",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
        isActive: false,
      },
    ]);
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

  test("maps accepted DM rows to reconnect instead of plain navigation", () => {
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

    expect(entries.map(callActivityDockSelection)).toEqual([
      {
        kind: "channel",
        channelId: "general",
        roomJid: "general@conference.example.com",
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
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
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
    });

    expect(html).toContain("Active calls");
    expect(html).toContain("General");
    expect(html).toContain("Bob");
    expect(html).toContain("Group call");
    expect(html).toContain("Video call");
    expect(html).toContain("2 people");
    expect(html).toContain("Live");
    expect(html).toContain("Open");
    expect(html).toContain("Reconnect");
    expect(html).toContain('aria-label="Reconnect Bob call, Live"');
    expect(html).not.toContain("Join");
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
    expect(html).toContain("Open");
    expect(html).toContain("aria-label=\"Open general call, 2 people\"");
    expect(html).not.toContain("disabled");
  });

  test("renders incoming 1:1 call activity without a false answer affordance", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-call-1",
        media: { audio: true, video: false },
        state: "ringing",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
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

  test("is mounted in the desktop sidebar and visible mobile shell", () => {
    const readyShell = readFileSync(new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url), "utf8");
    const mobileDrawers = readFileSync(new URL("../src/components/chat/ChatMobileDrawers.vue", import.meta.url), "utf8");

    expect(readyShell).toContain("import CallActivityDock");
    expect(readyShell.match(/<CallActivityDock/g)?.length ?? 0).toBeGreaterThanOrEqual(2);
    expect(readyShell).toContain("class=\"call-activity-dock--mobile\"");
    expect(readyShell).toContain("@select-channel=\"onSelectChannelFromSidebar\"");
    expect(readyShell).toContain("@select-dm=\"selectDm\"");
    expect(readyShell).toContain("@reconnect-dm=\"reconnectDmFromDock\"");

    expect(mobileDrawers).not.toContain("CallActivityDock");
  });

  test("renders hydrated DM call controls with reconnect context", async () => {
    $dmCallActivities.set({
      "bob@example.com": {
        peerJid: "bob@example.com",
        sid: "dm-call-1",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-25T12:00:00.000Z",
      },
    });

    const html = await renderVueComponent("../src/components/calls/CallButton.vue", {
      peerBareJid: "bob@example.com",
    });

    expect(html).toContain("Video call live");
    expect(html).toContain("Reconnect voice call");
    expect(html).toContain("Reconnect video call");
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

async function renderCallActivityDock(props: Record<string, unknown>) {
  return renderVueComponent("../src/components/calls/CallActivityDock.vue", props);
}

async function renderVueComponent(path: string, props: Record<string, unknown>) {
  const component = await loadVueComponent(path);
  return renderToString(createSSRApp({ render: () => h(component, props) }));
}

async function loadVueComponent(path: string) {
  const filename = new URL(path, import.meta.url);
  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, { id: filename.pathname, inlineTemplate: true });

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
  return new URL("./helpers/vue-sfc-stub.ts", import.meta.url).href;
}
