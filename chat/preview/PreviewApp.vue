<script setup lang="ts">
import { watch } from "vue";
import AppShell from "@/components/AppShell.vue";
import RoutePageShell from "@/components/pages/RoutePageShell.vue";
import { connectionStore } from "@/lib/connection-store";
import { appController } from "@/stores/app-controller";
import { $xmppStatus } from "@/stores/xmpp-status";
import type { ChatAppController } from "@/shell/chat-app-controller";
import { seedInbox } from "./inbox-preview";
import { seedSettings } from "./settings-preview";
import { seedAdmin } from "./admin-preview";

/**
 * Scenario switch for /_preview. `screen` picks the state:
 *   signin-one | signin-many | signin-none | checking | unavailable
 *   home (default) | jump | drawer-nav | drawer-details
 *   offline | reconnecting | error | reconnected | superseded
 * The account menu and the homeserver editor are opened by clicking.
 */
const screen = new URLSearchParams(location.search).get("screen") ?? "home";
const SERVER = "https://waddle.test";
const providers = { one: [{ id: "github", kind: "oauth", display_name: "GitHub" }], many: [
  { id: "github", kind: "oauth", display_name: "GitHub" },
  { id: "codeberg", kind: "oidc", display_name: "Codeberg" },
  { id: "google", kind: "oidc", display_name: "Google" },
], none: [] };

Object.assign(connectionStore, { defaultServerUrl: SERVER, activeServerUrl: SERVER });
if (screen.startsWith("signin-")) {
  const kind = screen.slice(7) as keyof typeof providers;
  Object.assign(connectionStore, {
    appState: "signed-out",
    providers: providers[kind] ?? providers.one,
    appError: kind === "none" ? "This server has no sign-in providers configured." : "",
  });
} else if (screen === "checking") {
  connectionStore.appState = "loading";
} else if (screen === "unavailable") {
  Object.assign(connectionStore, { appState: "error", appError: "We couldn't reach waddle.test. Check your connection and try again." });
} else {
  Object.assign(connectionStore, {
    appState: "ready",
    session: {
      session_id: "preview", username: "ada", avatar_url: null, xmpp_localpart: "ada",
      jid: "ada@waddle.test", xmpp_websocket_url: "", is_expired: false, expires_at: null,
    },
  });
}

const room = (name: string) => `${name}@muc.waddle.test`;

function seed(c: ChatAppController) {
  c.waddles.waddles.value = [{ id: "s1", name: "Polar Station", role: "owner" }];
  c.waddles.channels.value = [
    ["general", "chat"], ["design", "chat"], ["huddle-lounge", "chat"], ["ideas", "forum"], ["releases", "chat"],
  ].map(([name, type], position) => ({ id: name!, name: name!, jid: room(name!), spaceId: "s1", channel_type: type, position }));
  c.waddles.groupDms.value = [{ id: "g1", roomJid: room("g1"), name: "Ada, Linus, Grace" }];
  c.dmConversations.conversations.value = [
    { peerJid: "linus@waddle.test", peerUsername: "linus", unreadCount: 2, presenceShow: "available" },
    { peerJid: "grace@waddle.test", peerUsername: "grace", unreadCount: 0, presenceShow: "away" },
    { peerJid: "ken@waddle.test", peerUsername: "ken", unreadCount: 0, presenceShow: "dnd" },
  ];
  const now = Date.now();
  c.channelUnread.inboxState.value = {
    channels: new Map(["design", "releases"].map((name, i) => [room(name), {
      partner: room(name), kind: "muc", lastStanzaId: `s${i}`, lastUpdated: now - i, unread: 3 + i * 9,
    }])),
    threads: new Map(),
  };
  c.messaging.mentionedChannelCounts.value = { [room("design")]: 1 };

  if (["offline", "reconnecting", "error", "reconnected", "superseded", "drawer-details"].includes(screen)) void c.selectChannel("general");
  if (screen === "jump") c.ui.showQuickSwitcher.value = true;
  if (screen === "drawer-nav") c.ui.showMobileNav.value = true;
  if (screen === "drawer-details") c.ui.showMobileDetails.value = true;
  if (screen === "superseded") $xmppStatus.set({ state: "offline", detail: "", kind: "superseded" });
  else if (["offline", "reconnecting", "error"].includes(screen)) {
    $xmppStatus.set({ state: screen as "offline", detail: "Connection lost" });
  } else if (screen === "reconnected") {
    $xmppStatus.set({ state: "reconnecting", detail: "" });
    setTimeout(() => $xmppStatus.set({ state: "online", detail: "" }), 200);
  } else {
    $xmppStatus.set({ state: "online", detail: "" });
  }
  // Area fixtures (each returns unless the screen has its prefix).
  seedInbox(c, screen);
  seedSettings(c, screen);
  seedAdmin(c, screen);
}

watch(appController, (c) => { if (c) seed(c); }, { flush: "post", immediate: true });
</script>

<template>
  <AppShell>
    <RoutePageShell route-id="home" />
  </AppShell>
</template>
