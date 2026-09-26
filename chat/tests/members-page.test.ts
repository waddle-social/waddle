import { describe, expect, test } from "bun:test";
import { computed, ref } from "vue";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import type { DmConversation, RosterContact } from "../src/lib/xmpp/types";

function contact(jid: string, presenceShow?: RosterContact["presenceShow"], name?: string): RosterContact {
  return { jid, username: jid.split("@")[0] ?? jid, name, subscription: "both", groups: [], presenceShow };
}

function conversation(peerJid: string, presenceShow?: DmConversation["presenceShow"]): DmConversation {
  return { peerJid, peerUsername: peerJid.split("@")[0] ?? peerJid, unreadCount: 0, presenceShow };
}

/**
 * The Members page mounts only after `openMembers()` / the `/members`
 * route cleared the focused channel, so the directory is always the
 * roster merged with DM peers. Only the controller fields the page reads
 * are faked here.
 */
function fakeController(contacts: RosterContact[], conversations: DmConversation[]) {
  return {
    connectionStore: { session: { jid: "me@example.com", username: "me" } },
    xmppClient: computed(() => null),
    messaging: { roomPresence: ref({}) },
    rosterContacts: { contacts: ref(contacts) },
    dmConversations: { conversations: ref(conversations) },
    membersWithAvatars: ref([]),
    displayedMemberState: ref("ready"),
    authorJidByNick: ref({}),
    avatarUrlByAuthor: ref({}),
    activeChannelRoomJid: ref(null),
    activeRoomChannel: ref(null),
    handleOpenDm: () => undefined,
  };
}

async function renderMembersPage(contacts: RosterContact[], conversations: DmConversation[]) {
  return renderVueComponent(
    "../src/components/community/pages/MembersPage.vue",
    { controller: fakeController(contacts, conversations) },
    import.meta.url,
  );
}

describe("MembersPage", () => {
  test("the headline counts the cards the grid shows, roster and DM peers alike", async () => {
    const html = await renderMembersPage(
      [contact("bob@example.com", "available", "Bob B"), contact("carol@example.com", "away")],
      [conversation("amy@example.com", "available"), conversation("dan@example.com", "offline"), conversation("eve@example.com")],
    );

    // Two roster contacts plus three DM peers: the H1 must not read "2 members".
    expect(html).toContain("5 people</h1>");
    expect(html).not.toContain("members</h1>");
    // "Here now" follows the shared around rule: away is not here.
    expect(html).toContain("2 here now.");
    expect(html).toContain('aria-label="Bob B, available"');
    expect(html).toContain('aria-label="amy, available"');
    expect(html).not.toContain('aria-label="carol, away"');
  });

  test("an empty directory reads 0 people", async () => {
    const html = await renderMembersPage([], []);
    expect(html).toContain("0 people</h1>");
    expect(html).toContain("No contacts yet.");
  });
});
