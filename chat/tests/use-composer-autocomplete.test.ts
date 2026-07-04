import { describe, expect, test, mock } from "bun:test";
import { effectScope } from "vue";
import { useComposerAutocomplete } from "../src/components/chat/composables/use-composer-autocomplete";
import type { MentionCandidate } from "../src/lib/mentions";
import type { DiscoveredExtensionCommand } from "../src/lib/xmpp/extension-commands";
import type { SlashInvocation } from "../src/lib/slash-dispatch";

interface ChainRecord {
  focused: boolean;
  insertContentAt?: { range: { from: number; to: number }; content: string };
  setTextSelection?: { from: number; to: number };
  insertContent?: string;
}

/**
 * Minimal ProseMirror/TipTap stand-in: a doc made of paragraphs, a
 * collapsed cursor at (paragraph, offset), and a recording command chain.
 */
function makeEditor(paragraphs: string[], cursor: { para: number; offset: number }) {
  let pos = 1;
  for (let i = 0; i < cursor.para; i += 1) pos += (paragraphs[i]?.length ?? 0) + 2;
  pos += cursor.offset;

  const chains: ChainRecord[] = [];
  const doc = {
    firstChild: paragraphs.length > 0
      ? {
          type: { name: "paragraph" },
          textContent: paragraphs[0],
          nodeSize: paragraphs[0].length + 2,
        }
      : null,
    textBetween(from: number, to: number, blockSeparator = "\n") {
      const before = paragraphs.slice(0, cursor.para);
      const current = paragraphs[cursor.para]?.slice(0, cursor.offset) ?? "";
      void from;
      void to;
      return [...before, current].join(blockSeparator);
    },
  };
  const editor = {
    state: { selection: { empty: true, from: pos }, doc },
    chain() {
      const record: ChainRecord = { focused: false };
      const api = {
        focus() {
          record.focused = true;
          return api;
        },
        insertContentAt(range: { from: number; to: number }, content: string) {
          record.insertContentAt = { range, content };
          return api;
        },
        setTextSelection(range: { from: number; to: number }) {
          record.setTextSelection = range;
          return api;
        },
        insertContent(content: string) {
          record.insertContent = content;
          return api;
        },
        run() {
          chains.push(record);
        },
      };
      return api;
    },
  };
  return { editor, chains, pos };
}

function candidate(username: string): MentionCandidate {
  return { username, jid: `${username}@example.com`, avatar_url: null, kind: "member" };
}

function command(overrides: Partial<DiscoveredExtensionCommand> = {}): DiscoveredExtensionCommand {
  return {
    serviceJid: "ext.example.com",
    node: "poll#create",
    name: "Create poll",
    scope: "global",
    composerPrefix: "poll",
    ...overrides,
  };
}

function makeHarness(options: {
  editor?: ReturnType<typeof makeEditor>["editor"] | null;
  mentions?: MentionCandidate[];
  commands?: DiscoveredExtensionCommand[];
  inMuc?: boolean;
  slashSubmitBlocked?: boolean;
  dispatcher?: (invocation: SlashInvocation) => Promise<boolean>;
} = {}) {
  const scope = effectScope();
  let api: ReturnType<typeof useComposerAutocomplete> | undefined;
  scope.run(() => {
    api = useComposerAutocomplete({
      getTiptapEditor: () => options.editor ?? null,
      mentionCandidates: () => options.mentions ?? [],
      slashCommands: () => options.commands ?? [],
      inMuc: () => options.inMuc ?? true,
      slashSubmitBlocked: () => options.slashSubmitBlocked ?? false,
      dispatchSlashCommand: () => options.dispatcher,
    });
  });
  if (!api) throw new Error("composable did not initialize");
  return { api, stop: () => scope.stop() };
}

describe("useComposerAutocomplete mention trigger", () => {
  test("arms on @prefix, filters candidates, and inserts over the trigger range", () => {
    const { editor, chains, pos } = makeEditor(["hey @al"], { para: 0, offset: 7 });
    const { api, stop } = makeHarness({
      editor,
      mentions: [candidate("alice"), candidate("bob")],
    });

    api.checkAutocompleteFromEditor();
    expect(api.showMentions.value).toBe(true);
    expect(api.mentionResults.value.map((c) => c.username)).toEqual(["alice"]);
    expect(api.autocompleteAction.value).toBe("select-mention");

    api.insertMention(api.mentionResults.value[0]);
    expect(chains).toHaveLength(1);
    expect(chains[0].focused).toBe(true);
    expect(chains[0].insertContentAt).toEqual({
      range: { from: pos - 3, to: pos },
      content: "@alice ",
    });
    expect(api.showMentions.value).toBe(false);
    stop();
  });

  test("matches diacritic usernames against their plain-ASCII query", () => {
    const { editor } = makeEditor(["@an"], { para: 0, offset: 3 });
    const { api, stop } = makeHarness({ editor, mentions: [candidate("Ángela")] });
    api.checkAutocompleteFromEditor();
    expect(api.mentionResults.value.map((c) => c.username)).toEqual(["Ángela"]);
    stop();
  });

  test("a non-empty selection clears every popover", () => {
    const { editor } = makeEditor(["hey @al"], { para: 0, offset: 7 });
    const { api, stop } = makeHarness({ editor, mentions: [candidate("alice")] });
    api.checkAutocompleteFromEditor();
    expect(api.showMentions.value).toBe(true);

    (editor.state.selection as { empty: boolean }).empty = false;
    api.checkAutocompleteFromEditor();
    expect(api.showMentions.value).toBe(false);
    stop();
  });
});

describe("useComposerAutocomplete emoji trigger", () => {
  test("arms only once the shortcode query has two characters", () => {
    const short = makeEditor(["ok :s"], { para: 0, offset: 5 });
    const { api: apiShort, stop: stopShort } = makeHarness({ editor: short.editor });
    apiShort.checkAutocompleteFromEditor();
    expect(apiShort.showEmoji.value).toBe(false);
    stopShort();

    const long = makeEditor(["ok :smile"], { para: 0, offset: 9 });
    const { api, stop } = makeHarness({ editor: long.editor });
    api.checkAutocompleteFromEditor();
    expect(api.showEmoji.value).toBe(true);
    expect(api.emojiResults.value.length).toBeGreaterThan(0);
    stop();
  });
});

describe("useComposerAutocomplete slash trigger", () => {
  test("arms on a first-paragraph /prefix and expands a picked candidate", () => {
    const { editor, chains } = makeEditor(["/po"], { para: 0, offset: 3 });
    const { api, stop } = makeHarness({ editor, commands: [command()] });

    api.checkAutocompleteFromEditor();
    expect(api.showSlash.value).toBe(true);
    expect(api.slashPrefix.value).toBe("po");
    expect(api.slashCandidates.value.map((c) => c.composerPrefix)).toEqual(["poll"]);
    expect(api.autocompleteAction.value).toBe("select-command");

    api.selectAutocompleteResult();
    expect(chains).toHaveLength(1);
    expect(chains[0].setTextSelection).toEqual({ from: 1, to: 1 + 1 + 2 });
    expect(chains[0].insertContent).toBe("/poll ");
    expect(api.slashPrefix.value).toBe("poll");
    stop();
  });

  test("channel-scoped commands are hidden outside MUCs", () => {
    const { editor } = makeEditor(["/po"], { para: 0, offset: 3 });
    const { api, stop } = makeHarness({
      editor,
      commands: [command({ scope: "channel" })],
      inMuc: false,
    });
    api.checkAutocompleteFromEditor();
    expect(api.slashCandidates.value).toEqual([]);
    expect(api.slashBlocked.value).toBe(true);
    expect(api.autocompleteAction.value).toBe("block-slash");
    // Blocked slash holds the message instead of sending it.
    expect(api.selectAutocompleteResult()).toBe(true);
    stop();
  });

  test("does not arm when the cursor sits in a later paragraph", () => {
    const { editor } = makeEditor(["/poll", "notes"], { para: 1, offset: 5 });
    const { api, stop } = makeHarness({ editor, commands: [command()] });
    api.checkAutocompleteFromEditor();
    expect(api.showSlash.value).toBe(false);
    stop();
  });

  test("Escape dismissal survives re-checks until the /prefix disappears", () => {
    const { editor } = makeEditor(["/poll"], { para: 0, offset: 5 });
    const { api, stop } = makeHarness({ editor, commands: [command()] });
    api.checkAutocompleteFromEditor();
    expect(api.showSlash.value).toBe(true);

    api.dismissSlash();
    api.checkAutocompleteFromEditor();
    expect(api.showSlash.value).toBe(false);
    stop();
  });

  test("submit-slash dispatches the resolved command with its trailing args", async () => {
    const { editor } = makeEditor(["/poll cats or dogs"], { para: 0, offset: 18 });
    const invocations: SlashInvocation[] = [];
    const dispatcher = mock(async (invocation: SlashInvocation) => {
      invocations.push(invocation);
      return true;
    });
    const { api, stop } = makeHarness({ editor, commands: [command()], dispatcher });

    api.checkAutocompleteFromEditor();
    expect(api.autocompleteAction.value).toBe("submit-slash");
    expect(api.selectAutocompleteResult()).toBe(true);
    expect(api.showSlash.value).toBe(false);

    await Promise.resolve();
    expect(dispatcher).toHaveBeenCalledTimes(1);
    expect(invocations[0]?.command.composerPrefix).toBe("poll");
    stop();
  });

  test("a required-but-missing forum title holds slash dispatch", () => {
    const { editor } = makeEditor(["/poll cats"], { para: 0, offset: 10 });
    const dispatcher = mock(async () => true);
    const { api, stop } = makeHarness({
      editor,
      commands: [command()],
      dispatcher,
      slashSubmitBlocked: true,
    });
    api.checkAutocompleteFromEditor();
    expect(api.selectAutocompleteResult()).toBe(true);
    expect(dispatcher).not.toHaveBeenCalled();
    stop();
  });
});

describe("useComposerAutocomplete keyboard navigation", () => {
  function keyEvent(key: string): KeyboardEvent {
    return {
      key,
      shiftKey: false,
      ctrlKey: false,
      metaKey: false,
      altKey: false,
      preventDefault() {},
      stopPropagation() {},
    } as unknown as KeyboardEvent;
  }

  test("arrows wrap through the active result list", () => {
    const { editor } = makeEditor(["@"], { para: 0, offset: 1 });
    const { api, stop } = makeHarness({
      editor,
      mentions: [candidate("alice"), candidate("bob")],
    });
    api.checkAutocompleteFromEditor();

    api.onKeydown(keyEvent("ArrowDown"));
    expect(api.selectedIndex.value).toBe(1);
    api.onKeydown(keyEvent("ArrowDown"));
    expect(api.selectedIndex.value).toBe(0);
    api.onKeydown(keyEvent("ArrowUp"));
    expect(api.selectedIndex.value).toBe(1);
    stop();
  });

  test("Tab selects the highlighted candidate", () => {
    const { editor, chains } = makeEditor(["@"], { para: 0, offset: 1 });
    const { api, stop } = makeHarness({
      editor,
      mentions: [candidate("alice"), candidate("bob")],
    });
    api.checkAutocompleteFromEditor();
    api.onKeydown(keyEvent("ArrowDown"));
    api.onKeydown(keyEvent("Tab"));
    expect(chains[0]?.insertContentAt?.content).toBe("@bob ");
    stop();
  });
});
