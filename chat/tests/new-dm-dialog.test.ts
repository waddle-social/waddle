import { describe, expect, mock, test } from "bun:test";
import type { Ref } from "vue";
import type { UserSearchResult } from "../src/lib/chat-types";
import { setupVueComponent } from "./helpers/render-vue-sfc";

const account: UserSearchResult = {
  id: "chat@example.com", jid: "chat@example.com", username: "chat", display_name: null, avatar_url: null,
};

interface DialogBindings {
  open: Ref<boolean>;
  query: Ref<string>;
  results: Ref<UserSearchResult[]>;
  selectedJid: Ref<string | null>;
  isSearching: Ref<boolean>;
  searchError: Ref<string>;
  handleSubmit: () => void;
}

async function dialog(searchRecipients: (query: string) => Promise<UserSearchResult[]>) {
  const emit = mock(() => {});
  const bindings = await setupVueComponent(
    "../src/components/modals/NewDmDialog.vue", { open: true, searchRecipients }, import.meta.url, emit,
  ) as unknown as DialogBindings;
  bindings.open.value = true;
  return { ...bindings, emit };
}

const searchDelay = () => new Promise((resolve) => setTimeout(resolve, 250));

describe("New DM account selection", () => {
  test("text cannot submit; selecting a directory result emits its full JID", async () => {
    const search = mock(async () => [account]);
    const h = await dialog(search);
    try {
      h.query.value = "chat@example.com";
      h.handleSubmit();
      expect(h.emit).not.toHaveBeenCalled();
      expect(h.isSearching.value).toBe(true);
      await searchDelay();
      expect(search).toHaveBeenCalledWith("chat@example.com");
      expect(h.results.value).toEqual([account]);
      h.handleSubmit();
      expect(h.emit).not.toHaveBeenCalled();
      h.selectedJid.value = account.jid;
      h.handleSubmit();
      expect(h.emit).toHaveBeenCalledWith("submit", "chat@example.com");
      expect(h.open.value).toBe(false);
    } finally {
      h.open.value = false;
    }
  });

  test("a new query clears selection immediately and ignores stale results", async () => {
    let finish!: (users: UserSearchResult[]) => void;
    const search = mock(async (): Promise<UserSearchResult[]> => [account]);
    const h = await dialog(search);
    try {
      h.query.value = "chat";
      await searchDelay();
      h.selectedJid.value = account.jid;
      search.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
      h.query.value = "old";
      expect(h.selectedJid.value).toBeNull();
      expect(h.results.value).toEqual([]);
      await searchDelay();
      h.query.value = "missing";
      search.mockResolvedValue([]);
      finish([account]);
      await searchDelay();
      expect(h.results.value).toEqual([]);
      expect(h.isSearching.value).toBe(false);
      h.handleSubmit();
      expect(h.emit).not.toHaveBeenCalled();
      expect(h.open.value).toBe(true);
    } finally {
      h.open.value = false;
    }
  });

  test("search failure remains visible and the dialog stays open", async () => {
    const h = await dialog(async () => { throw new Error("Directory unavailable"); });
    try {
      h.query.value = "chat";
      await searchDelay();
      expect(h.searchError.value).toBe("Directory unavailable");
      expect(h.results.value).toEqual([]);
      h.handleSubmit();
      expect(h.emit).not.toHaveBeenCalled();
      expect(h.open.value).toBe(true);
    } finally {
      h.open.value = false;
    }
  });
});
