<script setup lang="ts">
import { watch } from "vue";
import AppShell from "@/components/AppShell.vue";
import RoutePageShell from "@/components/pages/RoutePageShell.vue";
import { connectionStore } from "@/lib/connection-store";
import { appController } from "@/stores/app-controller";
import { $xmppStatus } from "@/stores/xmpp-status";
import type { ChatAppController } from "@/shell/chat-app-controller";
import type { TimelineMessage } from "@/lib/chat-ui";
import { $pinnedRooms } from "@/stores/pinned-messages";
import { seedCallPreview } from "./call-fixtures";
import CallErrorToast from "@/components/calls/CallErrorToast.vue";
import IncomingCallToast from "@/components/calls/IncomingCallToast.vue";
import OutgoingCallToast from "@/components/calls/OutgoingCallToast.vue";

/**
 * Scenario switch for /_preview. `screen` picks the state:
 *   signin-one | signin-many | signin-none | checking | unavailable
 *   home (default) | jump | drawer-nav | drawer-details
 *   offline | reconnecting | error | reconnected | superseded
 *   room | room-pinned | room-loading | room-quiet | room-forum
 *   room-unavailable | room-pick | dm | dm-group | dm-quiet | dms
 *   huddle-* | call-* (see call-fixtures.ts)
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
  if (ROOM_SCREENS.includes(screen)) seedRoom(c);
  if (screen === "room-pick") c.ui.activePage.value = "chat";
  if (screen.startsWith("dm")) seedDm(c);
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
  if (screen.startsWith("huddle-") || screen.startsWith("call-")) seedCallPreview(c, screen);
  if (["huddle-room", "huddle-live", "huddle-expanded"].includes(screen)) seedHuddleThread(c);
  if (screen === "call-ended") seedEndedCall(c);
}

// A live #design huddle anchor with replies on its thread (Huddle-Join, HuddlePanel).
function seedHuddleThread(c: ChatAppController) {
  void c.selectChannel("design").then(() => {
    const started = new Date(Date.now() - 12 * 60_000).toISOString();
    c.messaging.messages.value = [
      msg("h0", "kim", 40, "Mockups for the huddle panel are in the design file."),
      msg("h1", "linus", 12, "linus started a huddle", {
        threadId: "pv-design-thread",
        callThread: { kind: "muc", sid: "pv-design", media: ["audio", "video"], initiator: "linus", started },
      }),
      msg("h2", "grace", 9, "Apple build is uploading, ten minutes.", { threadId: "pv-design-thread" }),
      msg("h3", "ken", 7, "Checklist is in docs/release.md", { threadId: "pv-design-thread" }),
      msg("h4", "kim", 3, "Sharing my screen in a sec."),
    ];
  });
}

// Huddle-DM: a declined call sits in the direct-message timeline as a pill row.
function seedEndedCall(c: ChatAppController) {
  void c.selectDm("linus@waddle.test").then(() => {
    const d = c.dmMessaging;
    d.isLoadingMessages.value = false;
    d.messages.value = [
      msg("e1", "linus", 30, "Got a minute for the release notes?"),
      msg("e2", "ada", 20, "linus started a call", {
        threadId: "pv-end",
        callThread: { kind: "dm", sid: "pv-end", media: ["audio"], initiator: "ada", outcome: "declined" },
      }),
      msg("e3", "linus", 18, "Sorry, on the train. Text is fine."),
    ];
  });
}

const ROOM_SCREENS = ["room", "room-pinned", "room-loading", "room-quiet", "room-forum", "room-unavailable"];

function msg(id: string, author: string, minutesAgo: number, body: string, extra: Partial<TimelineMessage> = {}): TimelineMessage {
  const createdAt = new Date(Date.now() - minutesAgo * 60_000).toISOString();
  return { id, stanzaId: id, replyableId: id, reactionTargetId: id, author, body, createdAt, createdAtSource: "archive", isSelf: author === "ada", ...extra };
}

function seedRoom(c: ChatAppController) {
  const m = c.messaging;
  void c.selectChannel(screen === "room-forum" ? "ideas" : "general").then(() => {
    m.isLoadingMessages.value = screen === "room-loading";
    if (screen === "room-unavailable") c.ui.actionError.value = "Messages could not be loaded.";
    if (screen !== "room" && screen !== "room-pinned") return;
    m.roomPresence.value = { mika: "available", jo: "available", priya: "away", theo: "available", ada: "available" };
    m.messages.value = [
      msg("m1", "mika", 40, "Server tests are green on `main`. Tagging the RC after the huddle.", { reactions: { "🐧": ["ada", "jo", "priya", "theo"] } }),
      msg("m2", "mika", 39, "Changelog draft is in the PR description.", { reactions: { "🐧": ["ada", "jo", "priya", "theo"], "👀": ["jo"] } }),
      msg("m3", "jo", 22, "@ada can you check MUC history on Android before we tag?", { mentions: ["ada@waddle.test"] }),
      msg("t1", "ada", 18, "On it. Pixel 9 and the emulator.", { threadId: "m3" }),
      msg("t2", "ada", 6, "Both fine. Unstamped rows now sort by arrival.", { threadId: "m3" }),
      msg("m4", "priya", 20, "I'll take the Apple build. Ana, ping me if Android history looks off."),
      msg("m5", "ada", 2, "Thanks! Release notes go out with the tag.", { isEdited: true }),
    ];
    m.firstUnseenId.value = "m3";
    m.typingUsers.value = ["theo"];
    if (screen === "room-pinned") {
      const at = new Date(Date.now() - 3_600_000).toISOString();
      $pinnedRooms.set(new Map([[room("general"), { hydrated: true, entries: [
        { target_stanza_id: "x1", pinner_jid: "mika", pinned_at: at, preview: { author_nick: "mika", message_timestamp: at, text: "Release checklist lives in docs/release.md. Tick it in order." } },
        { target_stanza_id: "x2", pinner_jid: "jo", pinned_at: at, preview: { author_nick: "jo", message_timestamp: at, text: "Code freeze Thursday 18:00 CET." } },
      ] } as never]]));
      c.ui.showPinnedPanel.value = true;
      c.activateRightPanel("pinned");
    }
  });
}

function seedDm(c: ChatAppController) {
  if (screen === "dms") { c.openDmList(); return; }
  if (screen === "dm-group") {
    c.waddles.groupDms.value = [{ id: "g1", roomJid: room("g1"), name: "Kim, Sam, Noor" }];
    c.waddles.channels.value = [...c.waddles.channels.value, { id: "g1", name: "Kim, Sam, Noor", jid: room("g1"), isGroupDm: true } as never];
    void c.selectGroupDm(room("g1")).then(() => {
      c.messaging.messages.value = [
        msg("g1", "kim", 30, "Planning the meetup. Thursday or Friday?"),
        msg("g2", "sam", 28, "Friday works for me"),
        msg("g3", "ada", 27, "Friday too. I'll bring the stickers."),
        msg("g4", "noor", 23, "Friday it is.", { reactions: { "🎉": ["ada", "kim"] } }),
      ];
    });
    return;
  }
  void c.selectDm("linus@waddle.test").then(() => {
    const d = c.dmMessaging;
    d.isLoadingMessages.value = false;
    if (screen === "dm-quiet") return;
    d.messages.value = [
      msg("d1", "linus", 64, "Morning! Did the Android build land?"),
      msg("d2", "linus", 62, "Coffee before the call?"),
      msg("d3", "ada", 61, "Build landed, history looks right. Yes, five minutes. Cocoa for me."),
      msg("d4", "linus", 60, "Of course it is.", { reactions: { "🐧": ["ada"] } }),
    ];
    d.typingUsers.value = ["linus"];
  });
}

watch(appController, (c) => { if (c) seed(c); }, { flush: "post", immediate: true });
</script>

<template>
  <AppShell>
    <RoutePageShell route-id="home" />
    <!-- XmppProvider hosts these in the app; the preview has no provider. -->
    <IncomingCallToast />
    <OutgoingCallToast />
    <CallErrorToast />
  </AppShell>
</template>
