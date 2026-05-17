// Tests for the XEP-0292 vcard4 profile editor surface.
//
// The Vue component itself is not mounted (no @vue/test-utils harness in this
// repo) — instead we test:
//   - the vcard4-types helpers that own the boundary shape, and
//   - a small in-memory model that mirrors `VCardEditor.vue`'s read-on-mount,
//     publish-on-save, optimistic-update + rollback flow against a stubbed
//     `BrowserXmppClient.fetchVCard4` / `publishVCard4`.
//
// The model under test below is intentionally a tiny copy of the editor's
// state machine so the same flow can be asserted without Vue reactivity.

import { describe, expect, mock, test } from "bun:test";
import {
  draftFromProfile,
  profileFromDraft,
  profilesEqual,
  type VCard4Draft,
  type VCard4Profile,
} from "../src/lib/xmpp/vcard4-types";

// ---------------------------------------------------------------------------
// vcard4-types unit tests
// ---------------------------------------------------------------------------

describe("vcard4-types helpers", () => {
  test("draftFromProfile defaults absent fields to empty strings", () => {
    expect(draftFromProfile(null)).toEqual({
      fullName: "",
      nickname: "",
      pronouns: "",
      note: "",
      url: "",
    });
  });

  test("draftFromProfile round-trips an existing profile", () => {
    const profile: VCard4Profile = {
      fullName: "Romeo Montague",
      nickname: "Romeo",
      pronouns: "he/him",
      note: "Star-crossed",
      url: "https://romeo.example.com",
    };
    expect(draftFromProfile(profile)).toEqual({
      fullName: "Romeo Montague",
      nickname: "Romeo",
      pronouns: "he/him",
      note: "Star-crossed",
      url: "https://romeo.example.com",
    });
  });

  test("profileFromDraft trims whitespace and drops empty fields", () => {
    const draft: VCard4Draft = {
      fullName: "  Juliet  ",
      nickname: "",
      pronouns: "she/her",
      note: "   ",
      url: " https://juliet.example.com ",
    };
    expect(profileFromDraft(draft)).toEqual({
      fullName: "Juliet",
      pronouns: "she/her",
      url: "https://juliet.example.com",
    });
  });

  test("profileFromDraft on an empty draft yields {}", () => {
    const empty: VCard4Draft = {
      fullName: "",
      nickname: "",
      pronouns: "",
      note: "",
      url: "",
    };
    expect(profileFromDraft(empty)).toEqual({});
  });

  test("profilesEqual treats undefined fields and empty strings as equal", () => {
    expect(profilesEqual({}, {})).toBe(true);
    expect(profilesEqual({ fullName: "" }, {})).toBe(true);
    expect(profilesEqual({ fullName: "x" }, { fullName: "x" })).toBe(true);
    expect(profilesEqual({ fullName: "x" }, { fullName: "y" })).toBe(false);
    expect(
      profilesEqual({ pronouns: "they/them" }, { pronouns: "she/they" }),
    ).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Editor flow — mirrors VCardEditor.vue, no Vue mount required.
// ---------------------------------------------------------------------------

interface EditorClient {
  fetchVCard4: (jid: string) => Promise<VCard4Profile | null>;
  publishVCard4: (profile: VCard4Profile) => Promise<void>;
}

interface EditorState {
  draft: VCard4Draft;
  lastPersisted: VCard4Profile;
  feedbackTone: "muted" | "success" | "error";
  feedbackMessage: string;
}

function createEditor(client: EditorClient, selfJid: string) {
  const state: EditorState = {
    draft: draftFromProfile(null),
    lastPersisted: {},
    feedbackTone: "muted",
    feedbackMessage: "",
  };

  async function load() {
    const profile = await client.fetchVCard4(selfJid);
    const next = profile ?? {};
    state.lastPersisted = next;
    state.draft = draftFromProfile(next);
    state.feedbackTone = "muted";
    state.feedbackMessage = profile ? "loaded" : "empty";
  }

  async function save() {
    const next = profileFromDraft(state.draft);
    if (profilesEqual(next, state.lastPersisted)) {
      state.feedbackTone = "muted";
      state.feedbackMessage = "nothing to publish";
      return;
    }
    const previous = state.lastPersisted;
    // Optimistic update of the persisted snapshot.
    state.lastPersisted = next;
    try {
      await client.publishVCard4(next);
      state.feedbackTone = "success";
      state.feedbackMessage = "published";
    } catch (error) {
      state.lastPersisted = previous;
      state.draft = draftFromProfile(previous);
      state.feedbackTone = "error";
      state.feedbackMessage = error instanceof Error ? error.message : "failed";
    }
  }

  return { state, load, save };
}

describe("VCardEditor flow", () => {
  test("empty PEP node hydrates draft with empty fields", async () => {
    const fetchVCard4 = mock(async () => null);
    const publishVCard4 = mock(async () => undefined);
    const editor = createEditor({ fetchVCard4, publishVCard4 }, "alice@example.com");

    await editor.load();

    expect(fetchVCard4).toHaveBeenCalledTimes(1);
    expect(editor.state.draft).toEqual({
      fullName: "",
      nickname: "",
      pronouns: "",
      note: "",
      url: "",
    });
    expect(editor.state.lastPersisted).toEqual({});
    expect(editor.state.feedbackMessage).toBe("empty");
  });

  test("existing PEP node round-trips through the editor without data loss", async () => {
    const stored: VCard4Profile = {
      fullName: "Romeo Montague",
      nickname: "Romeo",
      pronouns: "he/him",
      note: "Star-crossed",
      url: "https://romeo.example.com",
    };
    const fetchVCard4 = mock(async () => stored);
    const publishVCard4 = mock(async (_profile: VCard4Profile) => undefined);
    const editor = createEditor({ fetchVCard4, publishVCard4 }, "romeo@example.com");

    await editor.load();
    // Resave with no changes — should be a no-op.
    await editor.save();
    expect(publishVCard4).not.toHaveBeenCalled();
    expect(editor.state.feedbackMessage).toBe("nothing to publish");

    // Now edit and save.
    editor.state.draft.note = "Updated bio";
    await editor.save();
    expect(publishVCard4).toHaveBeenCalledTimes(1);
    const publishedArg = (publishVCard4 as unknown as ReturnType<typeof mock>).mock.calls[0]![0];
    expect(publishedArg).toEqual({
      fullName: "Romeo Montague",
      nickname: "Romeo",
      pronouns: "he/him",
      note: "Updated bio",
      url: "https://romeo.example.com",
    });
    expect(editor.state.feedbackTone).toBe("success");
  });

  test("pronouns are forwarded to publishVCard4 (no-op on pre-#663 servers)", async () => {
    const fetchVCard4 = mock(async () => null);
    const publishVCard4 = mock(async (_profile: VCard4Profile) => undefined);
    const editor = createEditor({ fetchVCard4, publishVCard4 }, "alice@example.com");

    await editor.load();
    editor.state.draft.pronouns = "they/them";
    editor.state.draft.fullName = "Alice";
    await editor.save();

    const publishedArg = (publishVCard4 as unknown as ReturnType<typeof mock>).mock.calls[0]![0];
    expect(publishedArg.pronouns).toBe("they/them");
    expect(publishedArg.fullName).toBe("Alice");
  });

  test("save error rolls back draft + persisted state to the previous snapshot", async () => {
    const stored: VCard4Profile = {
      fullName: "Mercutio",
      nickname: "Merc",
    };
    const fetchVCard4 = mock(async () => stored);
    const publishVCard4 = mock(async () => {
      throw new Error("publish failed");
    });
    const editor = createEditor({ fetchVCard4, publishVCard4 }, "mercutio@example.com");

    await editor.load();
    editor.state.draft.fullName = "Lord Mercutio";
    await editor.save();

    expect(publishVCard4).toHaveBeenCalledTimes(1);
    expect(editor.state.feedbackTone).toBe("error");
    expect(editor.state.feedbackMessage).toBe("publish failed");
    // Both the persisted snapshot and the draft are reverted.
    expect(editor.state.lastPersisted).toEqual(stored);
    expect(editor.state.draft).toEqual(draftFromProfile(stored));
  });
});
