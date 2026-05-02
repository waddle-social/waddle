import type { TimelineMessage } from "@/lib/chat-ui";
import type { MentionCandidate } from "@/lib/mentions";

const AI_ASSISTANT_MENTION = "waddle";
export const AI_CHATBOT_FEATURE = "urn:waddle:ai-chatbot:1";

const AI_CHATBOT_COMMAND_PATTERNS = [
  /(?:^|[:/#-])ai-chatbot(?:$|[:/#-])/i,
  /\bai\s+chatbot\b/i,
  /\bask\s+ai\b/i,
];

export interface AiComposerCommand {
  command: string;
  title: string;
  description: string;
}

const AI_COMPOSER_COMMANDS: readonly AiComposerCommand[] = [
  {
    command: "/ai",
    title: "Ask Waddle",
    description: "Start an AI thread from this room.",
  },
];

export function aiComposerCommandResults(query: string, enabled = true): AiComposerCommand[] {
  if (!enabled) return [];
  const normalized = query.trim().replace(/^\/+/, "").toLowerCase();
  return AI_COMPOSER_COMMANDS.filter((candidate) =>
    candidate.command.slice(1).startsWith(normalized)
      || candidate.title.toLowerCase().includes(normalized),
  );
}

export function withAiAssistantMentionCandidate(
  candidates: readonly MentionCandidate[],
  enabled = true,
): MentionCandidate[] {
  if (!enabled) return [...candidates];
  if (candidates.some((candidate) => candidate.username.toLowerCase() === AI_ASSISTANT_MENTION)) {
    return [...candidates];
  }

  return [
    {
      username: AI_ASSISTANT_MENTION,
      jid: null,
      avatar_url: null,
      kind: "member",
    },
    ...candidates,
  ];
}

/**
 * Generic bot-reply thread auto-open: scans `messages` for the first
 * message that is *new* (not in `seenMessageIds`), comes from someone
 * else (not `isSelf`), is a thread reply (`threadId` set and different
 * from its own `id`), and whose thread root is a self-authored feed
 * message.  Returns the root's id so the caller can open that thread.
 *
 * This replaces the previous AI-specific pending-prompt mechanism and
 * works for any extension bot, not just the AI chatbot.
 */
export function findBotReplyThreadToOpen(
  messages: readonly TimelineMessage[],
  seenMessageIds: ReadonlySet<string>,
): string | undefined {
  for (const msg of messages) {
    if (seenMessageIds.has(msg.id)) continue;
    // Only thread replies from others.
    if (!msg.threadId || msg.isSelf || msg.id === msg.threadId) continue;
    // Thread root must be a self-authored feed message visible in the timeline.
    const root = messages.find(
      (m) => m.id === msg.threadId && m.isSelf && (!m.threadId || m.id === m.threadId),
    );
    if (root) return root.id;
  }
  return undefined;
}

interface ExtensionPaletteCommand {
  node: string;
  name: string;
}

function isV1HiddenExtensionCommand(command: ExtensionPaletteCommand): boolean {
  const haystack = `${command.node} ${command.name}`;
  return AI_CHATBOT_COMMAND_PATTERNS.some((pattern) => pattern.test(haystack));
}

export function filterV1ExtensionPaletteCommands<T extends ExtensionPaletteCommand>(
  commands: readonly T[],
): T[] {
  return commands.filter((command) => !isV1HiddenExtensionCommand(command));
}
