import { describe, expect, mock, test } from "bun:test";
import type { ComputedRef, Ref } from "vue";
import type { UserSearchResult } from "../src/lib/chat-types";
import { occupantJidDirectory } from "../src/lib/avatars/author-jid";
import { setupVueComponent } from "./helpers/render-vue-sfc";

const chat: UserSearchResult = {
  id: "chat@example.com", jid: "chat@example.com", username: "chat", display_name: null,
};
const bob: UserSearchResult = {
  id: "bob@example.com", jid: "bob@example.com", username: "bob", display_name: "Bob",
};

interface Person { jid: string; label: string }
interface DialogBindings {
  open: Ref<boolean>;
  query: Ref<string>;
  results: Ref<UserSearchResult[]>;
  picked: Ref<Person[]>;
  name: Ref<string>;
  candidates: ComputedRef<Person[]>;
  isGroup: ComputedRef<boolean>;
  isSearching: Ref<boolean>;
  searchError: Ref<string>;
  toggle: (person: Person) => void;
  onSearchEnter: (event: KeyboardEvent) => void;
  handleSubmit: () => void;
}

async function dialog(
  searchRecipients: (query: string) => Promise<UserSearchResult[]>,
  props: Record<string, unknown> = {},
) {
  const emit = mock(() => {});
  const bindings = await setupVueComponent(
    "../src/components/modals/NewDmDialog.vue", { open: true, searchRecipients, ...props }, import.meta.url, emit,
  ) as unknown as DialogBindings;
  bindings.open.value = true;
  return { ...bindings, emit };
}

const searchDelay = () => new Promise((resolve) => setTimeout(resolve, 250));
const enter = () => ({ preventDefault: () => {} }) as KeyboardEvent;

describe("New message: pick people", () => {
  test("text cannot submit; one picked directory result opens a DM with its full JID", async () => {
    const search = mock(async () => [chat]);
    const h = await dialog(search);
    try {
      h.query.value = "chat@example.com";
      h.handleSubmit();
      expect(h.emit).not.toHaveBeenCalled();
      await searchDelay();
      expect(search).toHaveBeenCalledWith("chat@example.com");
      h.handleSubmit();
      expect(h.emit).not.toHaveBeenCalled();
      h.toggle(h.candidates.value[0]!);
      h.handleSubmit();
      expect(h.emit).toHaveBeenCalledWith("submit", "chat@example.com");
      expect(h.open.value).toBe(false);
    } finally {
      h.open.value = false;
    }
  });

  test("picks survive a new query; stale results never become candidates", async () => {
    let finish!: (users: UserSearchResult[]) => void;
    const search = mock(async (): Promise<UserSearchResult[]> => [chat]);
    const h = await dialog(search);
    try {
      h.query.value = "chat";
      await searchDelay();
      h.toggle(h.candidates.value[0]!);
      search.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
      h.query.value = "old";
      expect(h.results.value).toEqual([]);
      expect(h.picked.value.map((p) => p.jid)).toEqual(["chat@example.com"]);
      await searchDelay();
      h.query.value = "missing";
      search.mockResolvedValue([]);
      finish([bob]);
      await searchDelay();
      expect(h.results.value).toEqual([]);
      expect(h.candidates.value).toEqual([]);
    } finally {
      h.open.value = false;
    }
  });

  test("two picks make a group; Enter picks the top match instead of submitting", async () => {
    const search = mock(async (query: string) => (query === "bob" ? [bob] : [chat]));
    const h = await dialog(search);
    try {
      h.query.value = "chat";
      await searchDelay();
      h.onSearchEnter(enter());
      expect(h.query.value).toBe("");
      h.query.value = "bob";
      await searchDelay();
      h.onSearchEnter(enter());
      expect(h.emit).not.toHaveBeenCalled();
      expect(h.isGroup.value).toBe(true);
      h.name.value = "  Ops  ";
      h.handleSubmit();
      expect(h.emit).toHaveBeenCalledWith("createGroup", {
        name: "Ops",
        memberJids: ["chat@example.com", "bob@example.com"],
      });
      // The shell closes the dialog once the group exists.
      expect(h.open.value).toBe(true);
    } finally {
      h.open.value = false;
    }
  });

  test("from a DM, one more pick makes a group; self, the seed and bots are never offered", async () => {
    occupantJidDirectory.recordBots(["helper@bots.example.com"]);
    const h = await dialog(async () => [], {
      selfJid: "ada@example.com/web",
      seed: { jid: "linus@example.com", label: "linus" },
      contacts: [
        { jid: "ada@example.com", name: "Ada" },
        { jid: "linus@example.com", name: "Linus" },
        { jid: "helper@bots.example.com", name: "Helper" },
        { jid: "grace@example.com", name: "Grace" },
      ],
    });
    try {
      expect(h.candidates.value).toEqual([{ jid: "grace@example.com", label: "Grace" }]);
      h.toggle(h.candidates.value[0]!);
      expect(h.isGroup.value).toBe(true);
      h.handleSubmit();
      // The name the placeholder shows is the name the group gets.
      expect(h.emit).toHaveBeenCalledWith("createGroup", { name: "linus, Grace", memberJids: ["grace@example.com"] });
    } finally {
      h.open.value = false;
      occupantJidDirectory.clear();
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
