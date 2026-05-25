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
        title: "General",
        participantCount: 3,
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

    expect(html).toContain("Calls");
    expect(html).toContain("General");
    expect(html).toContain("Bob");
    expect(html).toContain("2 people");
    expect(html).toContain("Live");
  });

  test("is mounted in the desktop sidebar and visible mobile shell", () => {
    const readyShell = readFileSync(new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url), "utf8");
    const mobileDrawers = readFileSync(new URL("../src/components/chat/ChatMobileDrawers.vue", import.meta.url), "utf8");

    expect(readyShell).toContain("import CallActivityDock");
    expect(readyShell.match(/<CallActivityDock/g)?.length ?? 0).toBeGreaterThanOrEqual(2);
    expect(readyShell).toContain("class=\"call-activity-dock--mobile\"");
    expect(readyShell).toContain("@select-channel=\"onSelectChannelFromSidebar\"");
    expect(readyShell).toContain("@select-dm=\"selectDm\"");

    expect(mobileDrawers).not.toContain("CallActivityDock");
  });

  test("keeps the dock bounded and hydrated DM call controls actionable", () => {
    const dock = readFileSync(new URL("../src/components/calls/CallActivityDock.vue", import.meta.url), "utf8");
    const callButton = readFileSync(new URL("../src/components/calls/CallButton.vue", import.meta.url), "utf8");

    expect(dock).toContain("max-height: min(40dvh, 18rem)");
    expect(dock).toContain("overflow-y: auto");
    expect(callButton).toContain("Hydrated peer activity alone stays actionable");
    expect(callButton).toContain("state.value.phase !== \"idle\" && state.value.phase !== \"ended\"");
    expect(callButton).not.toContain("|| hasPeerCallActivity.value");
  });
});

async function renderCallActivityDock(props: Record<string, unknown>) {
  const component = await loadCallActivityDockComponent();
  return renderToString(createSSRApp({ render: () => h(component, props) }));
}

async function loadCallActivityDockComponent() {
  const filename = new URL("../src/components/calls/CallActivityDock.vue", import.meta.url);
  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, { id: "call-activity-dock-test", inlineTemplate: true });

  const tempDir = mkdtempSync(join(tmpdir(), "waddle-call-activity-dock-"));
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
    const modulePath = join(tempDir, "CallActivityDock.mjs");
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
