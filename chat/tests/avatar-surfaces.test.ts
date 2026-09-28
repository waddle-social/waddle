import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import { avatarStore } from "../src/lib/avatars/avatar-store";
import { occupantJidDirectory } from "../src/lib/avatars/author-jid";
import { $mucCallParticipantOwners, clearMucCallParticipants, $mucCallParticipants } from "../src/lib/calls/muc-call-presence";
import type { DmConversation } from "../src/lib/xmpp/types";

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const avatarFor = (jid: string) => `data:image/png;base64,${btoa(jid)}`;

function conversation(peerJid: string, peerUsername: string, extra: Partial<DmConversation> = {}): DmConversation {
  return { peerJid, peerUsername, unreadCount: 0, presenceShow: "available", ...extra };
}

/** Seed the shared store the way a live session would: one fetch per retained JID. */
async function seedAvatars(jids: string[]) {
  avatarStore.setFetcher(async (jid) => (jids.includes(jid) ? avatarFor(jid) : null));
  const releases = jids.map((jid) => avatarStore.retain(jid));
  await flush();
  for (const release of releases) release();
}

beforeEach(() => {
  avatarStore.reset();
  occupantJidDirectory.clear();
});

afterEach(() => {
  avatarStore.reset();
  occupantJidDirectory.clear();
  clearMucCallParticipants();
});

describe("peer avatars on list surfaces", () => {
  test("a DM-list row renders the peer's avatar from the store", async () => {
    await seedAvatars(["bob@example.com"]);
    const html = await renderVueComponent(
      "../src/components/chat/DmPanel.vue",
      { activePeerJid: null, conversations: [conversation("bob@example.com", "bob"), conversation("carol@example.com", "carol")] },
      import.meta.url,
    );

    expect(html).toContain(`src="${avatarFor("bob@example.com")}"`);
    // No avatar known for carol: initials, never someone else's face.
    expect(html).not.toContain(avatarFor("carol@example.com"));
  });

  test("a MUC private-message row uses the room's disclosed real JID, not the room", async () => {
    occupantJidDirectory.record("room@muc.example.com", "dave", "dave@example.com");
    await seedAvatars(["dave@example.com"]);
    const html = await renderVueComponent(
      "../src/components/chat/DmPanel.vue",
      {
        activePeerJid: null,
        conversations: [conversation("room@muc.example.com/dave", "dave", {
          mucPm: true,
          mucPmRoomJid: "room@muc.example.com",
        })],
      },
      import.meta.url,
    );

    expect(html).toContain(`src="${avatarFor("dave@example.com")}"`);
  });

  test("a Home direct-message row renders the peer's avatar from the store", async () => {
    await seedAvatars(["bob@example.com"]);
    const html = await renderVueComponent(
      "../src/components/chat/HomeDashboard.vue",
      {
        spaces: [],
        channels: [],
        contacts: [],
        isLoading: false,
        channelUnreadMap: {},
        activeChannelJids: new Set(),
        dmConversations: [conversation("bob@example.com", "bob", { lastMessageBody: "hi", lastMessageAt: "2026-05-08T13:00:00Z" })],
      },
      import.meta.url,
    );

    expect(html).toContain(`src="${avatarFor("bob@example.com")}"`);
  });

  test("the call dock shows a channel call participant's avatar via their real JID", async () => {
    $mucCallParticipants.set({ "general@conference.example.com": ["alice", "bob"] });
    $mucCallParticipantOwners.set({
      "general@conference.example.com": [{ nick: "alice", realJid: "alice@example.com/web" }],
    });
    occupantJidDirectory.record("general@conference.example.com", "bob", "bob@example.com");
    await seedAvatars(["alice@example.com", "bob@example.com"]);

    const html = await renderVueComponent(
      "../src/components/calls/CallActivityDock.vue",
      {
        channels: [{ id: "general", name: "General", jid: "general@conference.example.com" }],
        conversations: [],
        activeChannelId: null,
        activePeerJid: null,
        sidebarMode: "channels",
        activeChannelJids: new Set<string>(),
        selfFullJid: "carol@example.com/web",
      },
      import.meta.url,
    );

    expect(html).toContain(`src="${avatarFor("alice@example.com")}"`);
    expect(html).toContain(`src="${avatarFor("bob@example.com")}"`);
  });
});

describe("call participant stacks", () => {
  test("Home call cards resolve participants through the Muji owner's real JID", async () => {
    $mucCallParticipants.set({ "general@conference.example.com": ["alice", "bob"] });
    $mucCallParticipantOwners.set({
      "general@conference.example.com": [{ nick: "alice", realJid: "alice@example.com/web" }],
    });
    // Bob only has a room disclosure; Alice has none, so only the owner JID can name her.
    occupantJidDirectory.record("general@conference.example.com", "bob", "bob@example.com");
    await seedAvatars(["alice@example.com", "bob@example.com"]);

    const html = await renderVueComponent(
      "../src/components/chat/HomeDashboard.vue",
      {
        spaces: [],
        channels: [{ id: "general", name: "General", jid: "general@conference.example.com", spaceId: "team" }],
        contacts: [],
        isLoading: false,
        channelUnreadMap: {},
        activeChannelJids: new Set(),
        dmConversations: [],
        callParticipantCounts: { "general@conference.example.com": 2 },
        callParticipants: { "general@conference.example.com": ["alice", "bob"] },
      },
      import.meta.url,
    );

    expect(html).toContain(`src="${avatarFor("alice@example.com")}"`);
    expect(html).toContain(`src="${avatarFor("bob@example.com")}"`);
  });

  test("the Rooms page uses the same call-participant resolver as the dock and banner", async () => {
    const { readFileSync } = await import("node:fs");
    const source = readFileSync(new URL("../src/components/community/pages/RoomsPage.vue", import.meta.url), "utf8");
    expect(source).toContain("jid: callParticipantAvatarJid(tile.roomJid, nick)");
    expect(source).not.toContain("roomOccupantAvatarJid");
  });
});

describe("own avatar", () => {
  const session = {
    session_id: "s",
    username: "me",
    avatar_url: "https://idp.example/me.png",
    xmpp_localpart: "me",
    jid: "me@example.com",
    xmpp_websocket_url: "wss://example.com/ws",
    is_expired: false,
    expires_at: null,
  };

  function renderOwnAvatar() {
    return renderVueComponent(
      "../src/components/ui/UserAvatar.vue",
      { name: session.username, jid: session.jid, fallbackSrc: session.avatar_url, size: "md" },
      import.meta.url,
    );
  }

  test("prefers the published XEP-0084 avatar over the sign-in provider's picture", async () => {
    await seedAvatars(["me@example.com"]);
    const html = await renderOwnAvatar();
    expect(html).toContain(`src="${avatarFor("me@example.com")}"`);
    expect(html).not.toContain("idp.example");
  });

  test("falls back to the sign-in picture only while the store has not resolved the JID", async () => {
    expect(await renderOwnAvatar()).toContain('src="https://idp.example/me.png"');
  });

  test("an own avatar removal clears to initials immediately, not back to the sign-in picture", async () => {
    await seedAvatars(["me@example.com"]);
    avatarStore.handleAvatarChanged("me@example.com");
    const html = await renderOwnAvatar();
    expect(html).not.toContain("<img");
  });

  test("ProfilePanel and UserSettingsPage render the own avatar by JID with the session picture as fallback", async () => {
    const { readFileSync } = await import("node:fs");
    for (const file of ["ProfilePanel.vue", "UserSettingsPage.vue"]) {
      const source = readFileSync(new URL(`../src/components/chat/${file}`, import.meta.url), "utf8");
      expect(source).not.toContain(':src="session.avatar_url"');
      expect(source).toContain(':jid="session.jid" :fallback-src="session.avatar_url"');
    }
  });
});
