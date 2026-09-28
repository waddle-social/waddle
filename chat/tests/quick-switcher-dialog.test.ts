import { describe, expect, mock, test } from "bun:test";
import { nextTick, ref, type Ref } from "vue";
import { setupVueComponent } from "./helpers/render-vue-sfc";

interface DialogBindings {
  open: Ref<boolean>;
  query: Ref<string>;
  results: Ref<{ id: string }[]>;
  onKeydown: (event: KeyboardEvent) => void;
}

function key(name: string, extra: Partial<KeyboardEvent> = {}): KeyboardEvent {
  return { key: name, isComposing: false, preventDefault: mock(() => {}), ...extra } as unknown as KeyboardEvent;
}

async function dialog() {
  const controller = {
    ui: { showQuickSwitcher: ref(true) },
    waddles: {
      sortedSpaces: ref([{ id: "s1", name: "Penguins" }]),
      sortedChannels: ref([{ id: "c1", name: "general", spaceId: "s1", jid: "general@muc.example.com" }]),
    },
    messaging: { mentionedChannelCounts: ref({}) },
    computedChannelUnreadMap: ref({}),
    groupDmConversations: ref([{ id: "g1", roomJid: "g1@groups.example.com", name: "Weekend plans" }]),
    dmConversations: { conversations: ref([{ peerJid: "bob@example.com", peerUsername: "bob", unreadCount: 0 }]) },
    rosterContacts: { contacts: ref([]) },
    selectChannel: mock(async () => {}),
    selectGroupDm: mock(async () => true),
    selectDm: mock(async () => {}),
    openHome: mock(() => {}),
    openRooms: mock(() => {}),
    openThreads: mock(() => {}),
    openUnread: mock(() => {}),
    openMembers: mock(() => {}),
    openCommunitySurface: mock(() => {}),
    openUserSettings: mock(() => {}),
  };
  const bindings = await setupVueComponent(
    "../src/components/modals/QuickSwitcherDialog.vue", { controller }, import.meta.url,
  ) as unknown as DialogBindings;
  return { ...bindings, controller };
}

describe("QuickSwitcherDialog", () => {
  test("Enter opens the best match and closes", async () => {
    const h = await dialog();
    h.query.value = "#gen";
    await nextTick();
    const enter = key("Enter");
    h.onKeydown(enter);
    expect(enter.preventDefault).toHaveBeenCalled();
    expect(h.controller.selectChannel).toHaveBeenCalledWith("c1", { roomJid: "general@muc.example.com" });
    expect(h.open.value).toBe(false);
  });

  test("arrow keys move the highlight and wrap to pages", async () => {
    const h = await dialog();
    h.onKeydown(key("ArrowDown"));
    h.onKeydown(key("Enter"));
    expect(h.controller.selectGroupDm).toHaveBeenCalledWith("g1@groups.example.com");
    await nextTick();

    h.open.value = true;
    await nextTick();
    h.onKeydown(key("ArrowUp"));
    h.onKeydown(key("Enter"));
    expect(h.controller.openUserSettings).toHaveBeenCalledTimes(1);
  });

  test("opens DMs and ignores Enter while an input method is composing", async () => {
    const h = await dialog();
    h.query.value = "bob";
    await nextTick();
    h.onKeydown(key("Enter", { isComposing: true }));
    expect(h.controller.selectDm).not.toHaveBeenCalled();
    h.onKeydown(key("Enter"));
    expect(h.controller.selectDm).toHaveBeenCalledWith("bob@example.com");
  });

  test("reopening starts from an empty query", async () => {
    const h = await dialog();
    h.query.value = "nothing matches this";
    await nextTick();
    expect(h.results.value).toHaveLength(0);
    h.open.value = false;
    await nextTick();
    h.open.value = true;
    await nextTick();
    expect(h.query.value).toBe("");
    expect(h.results.value.length).toBeGreaterThan(0);
  });
});
