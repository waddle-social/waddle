import { computed, ref } from "vue";
import { searchEmoji } from "@/lib/emoji";
import { getComposerAutocompleteAction } from "@/lib/reply-ux";
import type { MentionCandidate } from "@/lib/mentions";
import { parseSlashTrigger } from "@/lib/slash-trigger";
import {
  listSlashCandidates,
  resolveSlashTarget,
  slashCandidateName,
  type SlashCandidate,
} from "@/lib/slash-candidates";
import type { BuiltinSlashOutcome } from "@/lib/slash-builtins";
import { buildSlashInvocation, type SlashInvocation } from "@/lib/slash-dispatch";
import type { DiscoveredExtensionCommand } from "@/lib/xmpp/extension-commands";

/**
 * The composer's inline autocomplete engine: `@mention`, `:emoji:` and
 * `/slash` triggers detected from the TipTap caret position, popover
 * result lists with keyboard navigation, and the select/submit routing
 * that decides whether an Enter press inserts a completion, dispatches a
 * slash command, or falls through to a normal message send.
 *
 * Built-in slash commands (`/me`, `/shrug`, `/giphy`|`/gif`, `/away`,
 * `/active`, `/dnd`) are available in every composer and win over an
 * extension command with the same name. When one resolves on Enter the
 * composable hides the popover, calls `runBuiltinSlash(outcome)` and
 * reports the key as consumed; the composer owns every effect:
 * - `send`: send the current editor document rewritten by
 *   `rewriteBuiltinSendDoc(outcome.rewrite, doc)`, then clear it as a
 *   normal send would. Held (never called) while `slashSubmitBlocked()`.
 * - `open-gif-picker`: open the GIF picker with `outcome.query` and clear
 *   the typed command.
 * - `set-presence`: `pickPresence(outcome.pick)` and clear the composer.
 */
export function useComposerAutocomplete(input: {
  /** Underlying TipTap Editor instance (ProseMirror state + chains). */
  getTiptapEditor: () => any;
  mentionCandidates: () => MentionCandidate[];
  slashCommands: () => DiscoveredExtensionCommand[];
  inMuc: () => boolean;
  /** True while a required forum title is missing — holds slash dispatch. */
  slashSubmitBlocked: () => boolean;
  dispatchSlashCommand: () => ((invocation: SlashInvocation) => Promise<boolean>) | undefined;
  /** Performs a resolved built-in's effect (see the contract above). */
  runBuiltinSlash: (outcome: BuiltinSlashOutcome) => void;
}) {
  const showMentions = ref(false);
  const showEmoji = ref(false);
  const showSlash = ref(false);
  const slashPrefix = ref("");
  const slashTrailing = ref("");
  const dismissedSlashPrefix = ref<string | null>(null);
  const mentionQuery = ref("");
  const emojiQuery = ref("");
  const selectedIndex = ref(0);

  // Track the ProseMirror position range for the active autocomplete trigger
  const triggerRange = ref<{ from: number; to: number } | null>(null);

  function stripDiacritics(s: string): string {
    return s.normalize("NFD").replace(/[\u0300-\u036f]/g, "").toLowerCase();
  }

  const mentionResults = computed(() => {
    const q = mentionQuery.value.toLowerCase();
    const qNorm = stripDiacritics(mentionQuery.value);
    if (!q) return input.mentionCandidates().slice(0, 8);
    return input.mentionCandidates().filter((candidate) => {
      const lower = candidate.username.toLowerCase();
      return lower.includes(q) || stripDiacritics(candidate.username).includes(qNorm);
    }).slice(0, 8);
  });

  const emojiResults = computed(() => searchEmoji(emojiQuery.value));

  const slashContext = computed(() => ({ inMuc: input.inMuc() }));
  const slashCandidates = computed(() =>
    listSlashCandidates(slashPrefix.value, input.slashCommands(), slashContext.value),
  );
  const slashResolution = computed(() =>
    resolveSlashTarget(slashPrefix.value, slashTrailing.value, input.slashCommands(), slashContext.value),
  );
  const slashBlocked = computed(() =>
    showSlash.value && slashPrefix.value.length > 0 && !slashResolution.value && slashCandidates.value.length === 0,
  );

  const activeResults = computed(() => {
    if (showMentions.value) return mentionResults.value;
    if (showEmoji.value) return emojiResults.value;
    if (showSlash.value) return slashCandidates.value;
    return [];
  });

  const autocompleteAction = computed(() =>
    getComposerAutocompleteAction({
      showMentions: showMentions.value,
      mentionCount: mentionResults.value.length,
      showEmoji: showEmoji.value,
      emojiCount: emojiResults.value.length,
      showSlash: showSlash.value,
      slashHasPrefix: slashPrefix.value.length > 0,
      slashCandidateCount: slashCandidates.value.length,
      slashHasResolution: !!slashResolution.value,
    }),
  );

  function clearAutocomplete() {
    showMentions.value = false;
    showEmoji.value = false;
    showSlash.value = false;
    triggerRange.value = null;
  }

  /** Escape while slash mode is armed: hide the popover and remember the
   * dismissed prefix so re-checks don't immediately re-arm it. */
  function dismissSlash() {
    showSlash.value = false;
    dismissedSlashPrefix.value = slashPrefix.value;
  }

  function firstParagraphTextFromDoc(doc: any): string {
    const firstChild = doc?.firstChild;
    if (!firstChild || firstChild.type?.name !== "paragraph") return "";
    return firstChild.textContent ?? "";
  }

  function checkAutocompleteFromEditor() {
    // Access the underlying TipTap editor to get ProseMirror state
    const tiptapEditor = input.getTiptapEditor();
    if (!tiptapEditor?.state) return;

    const { selection, doc } = tiptapEditor.state;
    if (!selection.empty) {
      clearAutocomplete();
      return;
    }

    const pos = selection.from;
    const textBefore = doc.textBetween(0, pos, "\n", "\uFFFC");

    const mentionMatch = textBefore.match(/(?:^|\s)@(\S*)$/);
    if (mentionMatch) {
      mentionQuery.value = mentionMatch[1];
      selectedIndex.value = 0;
      showMentions.value = true;
      showEmoji.value = false;
      showSlash.value = false;
      triggerRange.value = {
        from: pos - mentionMatch[0].trimStart().length,
        to: pos,
      };
      return;
    }
    showMentions.value = false;

    const emojiMatch = textBefore.match(/(?:^|\s):([a-z0-9_+-]*)$/i);
    if (emojiMatch && emojiMatch[1].length >= 2) {
      emojiQuery.value = emojiMatch[1];
      selectedIndex.value = 0;
      showEmoji.value = true;
      showSlash.value = false;
      triggerRange.value = {
        from: pos - emojiMatch[0].trimStart().length,
        to: pos,
      };
      return;
    }
    showEmoji.value = false;

    // Slash autocomplete is anchored to paragraph 0; if the cursor is in a
    // later paragraph, an earlier `/word` must not arm the slash submit path.
    const firstChild = doc.firstChild;
    const cursorInFirstParagraph =
      !!firstChild && firstChild.type?.name === "paragraph" && pos > 0 && pos <= firstChild.nodeSize - 1;

    const firstParagraph = firstParagraphTextFromDoc(doc);
    const slash = parseSlashTrigger(firstParagraph);
    if (slash) {
      // Hold the popover when the cursor is outside paragraph 0 — but keep the
      // dismissedSlashPrefix intact so the user can navigate away and back
      // without re-arming the same `/word` they already dismissed.
      const suppressedByDismissal =
        dismissedSlashPrefix.value !== null && dismissedSlashPrefix.value === slash.prefix;
      if (!cursorInFirstParagraph || suppressedByDismissal) {
        showSlash.value = false;
      } else {
        slashPrefix.value = slash.prefix;
        slashTrailing.value = slash.trailing;
        selectedIndex.value = 0;
        showSlash.value = true;
        // The slash trigger spans the first paragraph from position 1 (after the
        // leading <p> open tag) through the prefix length.
        triggerRange.value = { from: 1, to: 1 + 1 + slash.prefix.length };
      }
      return;
    }

    // The leading `/word` is gone; safe to reset the dismissal too.
    dismissedSlashPrefix.value = null;
    showSlash.value = false;
    triggerRange.value = null;
  }

  function insertMention(candidate: MentionCandidate) {
    const tiptapEditor = input.getTiptapEditor();
    if (!tiptapEditor || !triggerRange.value) return;

    const replacement = `@${candidate.username} `;
    tiptapEditor.chain()
      .focus()
      .insertContentAt(triggerRange.value, replacement)
      .run();

    showMentions.value = false;
    triggerRange.value = null;
  }

  function insertEmoji(emoji: string) {
    const tiptapEditor = input.getTiptapEditor();
    if (!tiptapEditor || !triggerRange.value) return;

    const replacement = `${emoji} `;
    tiptapEditor.chain()
      .focus()
      .insertContentAt(triggerRange.value, replacement)
      .run();

    showEmoji.value = false;
    triggerRange.value = null;
  }

  function expandSlashCandidate(candidate: SlashCandidate) {
    const tiptapEditor = input.getTiptapEditor();
    const name = slashCandidateName(candidate);
    if (!tiptapEditor || !name) return;
    // If the user already typed a space after the partial prefix, swallow it
    // so we don't end up with double spaces (e.g. `/p ` + `/poll ` → `/poll  `).
    const firstParagraph = firstParagraphTextFromDoc(tiptapEditor.state.doc);
    const consumedExtra = firstParagraph.charAt(1 + slashPrefix.value.length) === " " ? 1 : 0;
    const replacement = `/${name} `;
    tiptapEditor.chain()
      .focus()
      .setTextSelection({ from: 1, to: 1 + 1 + slashPrefix.value.length + consumedExtra })
      .insertContent(replacement)
      .run();
    // Only the command word changes; any trailing text typed after it stays.
    slashPrefix.value = name;
    showSlash.value = true;
  }

  function runBuiltinResolution(outcome: BuiltinSlashOutcome): void {
    // Sending built-ins honour the forum-title gate like any other send.
    if (outcome.kind === "send" && input.slashSubmitBlocked()) return;
    showSlash.value = false;
    triggerRange.value = null;
    input.runBuiltinSlash(outcome);
  }

  function dispatchSlashResolution(): boolean {
    const resolution = slashResolution.value;
    if (!resolution) return false;
    if (resolution.kind === "builtin") {
      runBuiltinResolution(resolution.outcome);
      return true;
    }
    const command = resolution.command;
    // Forum channels demand a title; don't smuggle a slash dispatch past that gate.
    if (input.slashSubmitBlocked()) return true;
    const invocation = buildSlashInvocation(command, slashTrailing.value);
    const dispatcher = input.dispatchSlashCommand();
    // Hide the popover for the round-trip; only commit the dismissal once the
    // parent reports success, so a failed dispatch leaves slash mode armed for
    // the user to retry, edit, or Esc.
    showSlash.value = false;
    triggerRange.value = null;
    if (!dispatcher) return true;
    const dispatchedPrefix = slashPrefix.value;
    void (async () => {
      const ok = await dispatcher(invocation);
      if (ok) {
        dismissedSlashPrefix.value = dispatchedPrefix;
      }
    })();
    return true;
  }

  function selectAutocompleteResult(action = autocompleteAction.value): boolean {
    if (action === "select-mention") {
      insertMention(mentionResults.value[selectedIndex.value]);
      return true;
    }

    if (action === "select-emoji") {
      insertEmoji(emojiResults.value[selectedIndex.value].emoji);
      return true;
    }

    if (action === "select-command") {
      const candidate = slashCandidates.value[selectedIndex.value];
      if (candidate) expandSlashCandidate(candidate);
      return true;
    }

    if (action === "submit-slash") {
      return dispatchSlashResolution();
    }

    if (action === "block-slash") {
      // Hold the message; popover already shows an inline "no command" hint.
      return true;
    }

    return false;
  }

  /** Keys only drive the popover when typed in the editor itself; the
   * composer also hosts menus and inputs (the + menu, the link field,
   * GIF search) whose own keyboard handling must not be hijacked. */
  function isEditorKeyEvent(e: KeyboardEvent): boolean {
    const dom: HTMLElement | undefined = input.getTiptapEditor()?.view?.dom;
    if (!dom || !e.target) return true;
    return dom.contains(e.target as Node);
  }

  function onKeydown(e: KeyboardEvent) {
    if (!isEditorKeyEvent(e)) return;
    if (activeResults.value.length > 0 && (showMentions.value || showEmoji.value || showSlash.value)) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        e.stopPropagation();
        selectedIndex.value = (selectedIndex.value + 1) % activeResults.value.length;
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        e.stopPropagation();
        selectedIndex.value =
          (selectedIndex.value - 1 + activeResults.value.length) % activeResults.value.length;
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        e.stopPropagation();
        selectAutocompleteResult();
        return;
      }
      if (e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey) {
        e.preventDefault();
        e.stopPropagation();
        selectAutocompleteResult();
        return;
      }
    }
  }

  return {
    showMentions,
    showEmoji,
    showSlash,
    slashPrefix,
    slashBlocked,
    selectedIndex,
    mentionResults,
    emojiResults,
    slashCandidates,
    autocompleteAction,
    checkAutocompleteFromEditor,
    clearAutocomplete,
    dismissSlash,
    insertMention,
    insertEmoji,
    expandSlashCandidate,
    selectAutocompleteResult,
    onKeydown,
  };
}
