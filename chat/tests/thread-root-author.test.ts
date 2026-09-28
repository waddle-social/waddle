import { afterEach, describe, expect, test } from "bun:test";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import { threadRootAvatarJid } from "../src/lib/avatars/thread-root-author";
import { avatarStore } from "../src/lib/avatars/avatar-store";
import { occupantJidDirectory } from "../src/lib/avatars/author-jid";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { WasmThreadEntry } from "../src/lib/xmpp/wasm-types";

const ROOM = "general@conference.example.com";

const entry: WasmThreadEntry = {
  channel: ROOM,
  thread_id: "root-1",
  last_stanza_id: "s",
  last_activity: "2026-09-27T12:00:00Z",
  unread: 0,
  reply_count: 2,
  has_unread: false,
  root_author: "sam",
};

function root(overrides: Partial<TimelineMessage>): TimelineMessage {
  return {
    id: "root-1",
    author: "sam",
    authorJid: `${ROOM}/sam`,
    authorOccupantJid: `${ROOM}/sam`,
    body: "kick-off",
    createdAt: "2026-09-27T10:00:00Z",
    createdAtSource: "archive",
    isSelf: false,
    ...overrides,
  };
}

afterEach(() => {
  avatarStore.reset();
  occupantJidDirectory.clear();
});

describe("threads-list starter avatar", () => {
  test("uses the loaded root row's own author, even after the nick changed hands", () => {
    // The nick is Bob's now; the root row was stamped with Alice at ingest.
    occupantJidDirectory.record(ROOM, "sam", "bob@example.com");
    const jid = threadRootAvatarJid(entry, {
      loadedRoomJid: ROOM,
      resolveRoot: () => root({ authorAvatarJid: "alice@example.com" }),
    });
    expect(jid).toBe("alice@example.com");
  });

  test("uses the root row's archive real JID", () => {
    expect(threadRootAvatarJid(entry, {
      loadedRoomJid: ROOM,
      resolveRoot: () => root({ authorRealJid: "carol@example.com/phone" }),
    })).toBe("carol@example.com");
  });

  test("renders initials when the root row is not loaded or belongs to another room", () => {
    occupantJidDirectory.record(ROOM, "sam", "bob@example.com");
    expect(threadRootAvatarJid(entry, { loadedRoomJid: ROOM, resolveRoot: () => undefined })).toBeNull();
    expect(threadRootAvatarJid(entry, { loadedRoomJid: "other@conference.example.com", resolveRoot: () => root({ authorRealJid: "carol@example.com" }) }))
      .toBeNull();
    expect(threadRootAvatarJid(entry, { loadedRoomJid: null, resolveRoot: () => root({ authorRealJid: "carol@example.com" }) }))
      .toBeNull();
  });

  test("the row never reads a nick as a JID nor the current nick holder", async () => {
    occupantJidDirectory.record(ROOM, "sam@example.com", "bob@example.com");
    avatarStore.setFetcher(async (jid) => `data:${jid}`);
    const retainBob = avatarStore.retain("bob@example.com");
    const retainNickJid = avatarStore.retain("sam@example.com");
    await new Promise((resolve) => setTimeout(resolve, 0));
    retainBob();
    retainNickJid();

    // A nick that looks like a JID, with no loaded root: initials only.
    const html = await renderVueComponent(
      "../src/components/chat/ThreadsListRow.vue",
      { entry: { ...entry, root_author: "sam@example.com" } },
      import.meta.url,
    );
    expect(html).not.toContain("data:sam@example.com");
    expect(html).not.toContain("data:bob@example.com");

    const resolved = await renderVueComponent(
      "../src/components/chat/ThreadsListRow.vue",
      { entry, rootAuthorJid: "bob@example.com" },
      import.meta.url,
    );
    expect(resolved).toContain('src="data:bob@example.com"');
  });
});
