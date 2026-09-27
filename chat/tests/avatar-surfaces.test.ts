import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import { avatarStore } from "../src/lib/avatars/avatar-store";
import { occupantJidDirectory } from "../src/lib/avatars/author-jid";
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
});
