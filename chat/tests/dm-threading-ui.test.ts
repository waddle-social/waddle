import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createSSRApp, h } from "vue";
import { parse, compileScript } from "vue/compiler-sfc";
import { renderToString } from "vue/server-renderer";
import ts from "typescript";
import type { MessageThreadEntry } from "../src/channels/threads";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { DmConversation } from "../src/lib/xmpp-client";

describe("DM threading UI contract", () => {
  test("renders DM thread entries and selects them from the panel model", async () => {
    const thread = threadEntry("dm-thread-1", "Launch follow-up", 2);
    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [conversation()],
      activePeerJid: "alice@example.com",
      threadEntries: [thread, threadEntry("empty-thread", "Empty", 0)],
    });

    expect(html).toContain("Direct message threads");
    expect(html).toContain("Launch follow-up");
    expect(html).toContain("2 replies");
    expect(html).not.toContain("Open thread Empty");

    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [conversation()],
      activePeerJid: "alice@example.com",
      threadEntries: [thread],
    }, (...args) => {
      emitted.push(args);
    });

    setupBindingFunction(bindings, "emit")("selectThread", thread.threadId);
    expect(emitted).toEqual([["selectThread", "dm-thread-1"]]);
  });

  test("offers Add people from a 1:1 direct message", async () => {
    const html = await renderVueComponent("../src/components/chat/DmPanel.vue", {
      conversations: [conversation()],
      activePeerJid: "alice@example.com",
    });

    expect(html).toContain("Add people to Alice");
  });

  test("wires Add people clicks through desktop and mobile shells", () => {
    const dmPanel = readFileSync(new URL("../src/components/chat/DmPanel.vue", import.meta.url), "utf8");
    const readyShell = readFileSync(new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url), "utf8");
    const mobileDrawers = readFileSync(new URL("../src/components/chat/ChatMobileDrawers.vue", import.meta.url), "utf8");

    expect(dmPanel).toContain('@click.stop="emit(\'selectDm\', conversation.peerJid)"');
    expect(dmPanel).toContain('@click.stop="emit(\'addPeopleToDm\', conversation.peerJid)"');
    expect(readyShell).toContain('@add-people-to-dm="handleAddPeopleToDm"');
    expect(mobileDrawers).toContain('@add-people-to-dm="handleAddPeopleToDm"');
  });

  test("seeded group-DM submits include the original direct-message peer", () => {
    const controller = readFileSync(new URL("../src/shell/controllers/use-dm-sync.ts", import.meta.url), "utf8");
    const seedIndex = controller.indexOf("const seedPeerJid = ui.groupDmSeedPeerJid.value;");
    const payloadIndex = controller.indexOf("groupDmSpawnPayloadFromDm({", seedIndex);
    const peerIndex = controller.indexOf("peerJid: seedPeerJid", payloadIndex);
    const nameIndex = controller.indexOf("name: payload.name", payloadIndex);
    const membersIndex = controller.indexOf("selectedMemberJids: payload.memberJids", payloadIndex);
    const genericNameIndex = controller.indexOf('name: payload.name.trim() || payload.memberJids.map(groupDmMemberLabel).join(", ")', payloadIndex);
    const errorIndex = controller.indexOf('"Choose at least one more contact."', payloadIndex);
    const createIndex = controller.indexOf("client.createGroupDm(createPayload.name, createPayload.memberJids)", payloadIndex);
    const selectIndex = controller.indexOf("await selectGroupDm(created.roomJid);", createIndex);

    expect(seedIndex).toBeGreaterThan(-1);
    expect(payloadIndex).toBeGreaterThan(seedIndex);
    expect(peerIndex).toBeGreaterThan(payloadIndex);
    expect(nameIndex).toBeGreaterThan(payloadIndex);
    expect(membersIndex).toBeGreaterThan(payloadIndex);
    expect(genericNameIndex).toBeGreaterThan(payloadIndex);
    expect(errorIndex).toBeGreaterThan(payloadIndex);
    expect(createIndex).toBeGreaterThan(payloadIndex);
    expect(selectIndex).toBeGreaterThan(createIndex);
  });

  test("seeded Add people dialog requires one picked contact while generic group creation requires two", () => {
    const modals = readFileSync(new URL("../src/components/chat/ChatAppModals.vue", import.meta.url), "utf8");
    const dialog = readFileSync(new URL("../src/components/modals/NewGroupDmDialog.vue", import.meta.url), "utf8");

    expect(modals).toContain(':minimum-selected-contacts="ui.groupDmSeedPeerJid.value ? 1 : 2"');
    expect(modals).toContain(':excluded-jids="ui.groupDmSeedPeerJid.value ? [ui.groupDmSeedPeerJid.value] : []"');
    expect(dialog).toContain("selected.value.size >= (props.minimumSelectedContacts ?? 2)");
    expect(dialog).toContain("!excluded.has(bare)");
    expect(dialog).toContain('labelled-by="new-group-dm-title"');
    expect(dialog).toContain('id="new-group-dm-title"');
    expect(dialog).toContain('name: name.value.trim()');
    expect(dialog).toContain('(props.minimumSelectedContacts ?? 2) < 2 ? "Add people" : "Create group"');
  });

  test("restores DM route threads only after the async route request is still current", () => {
    const controller = readFileSync(new URL("../src/shell/controllers/use-route-sync.ts", import.meta.url), "utf8");
    const dmRouteStart = controller.indexOf('if (match.id === "dm") {');
    const openDmIndex = controller.indexOf("await openDm(`${username}@${domain}`);", dmRouteStart);
    const staleGuardIndex = controller.indexOf("if (requestId !== routeRequestId) return;", openDmIndex);
    const restoreIndex = controller.indexOf("activeThreadStack.value = match.search.thread;", staleGuardIndex);

    expect(dmRouteStart).toBeGreaterThan(-1);
    expect(openDmIndex).toBeGreaterThan(dmRouteStart);
    expect(staleGuardIndex).toBeGreaterThan(openDmIndex);
    expect(restoreIndex).toBeGreaterThan(staleGuardIndex);
  });
});

function conversation(): DmConversation {
  return {
    peerJid: "alice@example.com",
    peerUsername: "Alice",
    lastMessage: "See the thread",
    lastMessageAt: "2026-01-01T00:00:00.000Z",
  };
}

function threadEntry(threadId: string, body: string, count: number): MessageThreadEntry {
  const root: TimelineMessage = {
    id: threadId,
    body,
    nick: "alice",
    timestamp: 1,
    createdAt: "2026-01-01T00:00:00.000Z",
  } as TimelineMessage;
  const directChildren = Array.from({ length: count }, (_, index) => ({
    id: `${threadId}-reply-${index}`,
    body: `reply ${index}`,
    nick: "bob",
    timestamp: index + 2,
    createdAt: `2026-01-01T00:0${index + 1}:00.000Z`,
    threadId,
  })) as TimelineMessage[];
  return {
    threadId,
    root,
    directChildren,
    allDescendants: directChildren,
    count,
    lastTs: directChildren.at(-1)?.createdAt ?? root.createdAt,
  };
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
  return new URL("./helpers/vue-sfc-stub.ts", import.meta.url).href;
}
