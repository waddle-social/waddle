import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { createNotifySettingsStore } from "../src/lib/notify-settings";
import { renderVueComponent, setupVueComponent } from "./helpers/render-vue-sfc";

function contentAreaBaseProps() {
  return {
    draft: "",
    forumTitle: "",
    pinnedPanelOpen: false,
    waddle: { id: "team", name: "Team" },
    channel: { id: "general", name: "General", jid: "general@conference.example.com", spaceId: "team" },
    roomJid: "general@conference.example.com",
    dmPeer: null,
    sidebarMode: "channels",
    messages: [],
    firstUnseenId: null,
    xmppStatus: { state: "online", detail: "" },
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
    notifySettings: createNotifySettingsStore(),
    reactionMode: null,
  };
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

async function suppressVueLifecycleSetupWarnings<T>(fn: () => Promise<T>): Promise<T> {
  const originalWarn = console.warn;
  console.warn = (...args: unknown[]) => {
    const message = String(args[0] ?? "");
    if (
      message.includes("onMounted is called when there is no active component instance")
      || message.includes("onBeforeUnmount is called when there is no active component instance")
      || message.includes("onUnmounted is called when there is no active component instance")
      || message.includes("useModel() called without active instance")
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

describe("connection notice UI", () => {
  test("keeps the superseded reconnect banner wired in ContentArea source", () => {
    const source = readFileSync(new URL("../src/components/chat/ContentArea.vue", import.meta.url), "utf8");
    expect(source).toContain('v-if="connectionNotice.actionLabel"');
    expect(source).toContain('@click="handleConnectionNoticeAction"');
    expect(source).toContain("recoverSupersededSession()");
  });

  test("renders the superseded reconnect banner on channel surfaces", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/ContentArea.vue",
      {
        ...contentAreaBaseProps(),
        xmppStatus: { state: "offline", detail: "This session was resumed in another tab.", kind: "superseded" },
      },
      import.meta.url,
    );

    expect(html).toContain("Session resumed in another tab");
    expect(html).toContain("Reconnect to continue from this tab.");
    expect(html).toContain(">Reconnect<");
  });

  test("renders the superseded reconnect banner on DM surfaces", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/ContentArea.vue",
      {
        ...contentAreaBaseProps(),
        channel: null,
        roomJid: null,
        dmPeer: { peerJid: "bob@example.com", peerUsername: "Bob", presenceShow: "available" },
        sidebarMode: "dms",
        xmppStatus: { state: "offline", detail: "This session was resumed in another tab.", kind: "superseded" },
      },
      import.meta.url,
    );

    expect(html).toContain("Session resumed in another tab");
    expect(html).toContain("Reconnect to continue from this tab.");
    expect(html).toContain(">Reconnect<");
  });

  test("invokes recoverSupersededSession from the shared banner action", async () => {
    let recoverCalls = 0;
    const bindings = await suppressVueLifecycleSetupWarnings(() =>
      setupVueComponent(
        "../src/components/chat/ContentArea.vue",
        {
          ...contentAreaBaseProps(),
          xmppStatus: { state: "offline", detail: "This session was resumed in another tab.", kind: "superseded" },
          xmppClient: {
            async recoverSupersededSession() {
              recoverCalls += 1;
            },
          },
        },
        import.meta.url,
      )
    );

    await setupBindingFunction(bindings, "handleConnectionNoticeAction")();
    expect(recoverCalls).toBe(1);
  });

  test("ordinary offline rendering stays unchanged", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/ContentArea.vue",
      {
        ...contentAreaBaseProps(),
        xmppStatus: { state: "offline", detail: "" },
      },
      import.meta.url,
    );

    expect(html).toContain("Disconnected");
    expect(html).toContain("Any queued messages will send once you&#39;re connected again.");
    expect(html).not.toContain(">Reconnect<");
  });
});
