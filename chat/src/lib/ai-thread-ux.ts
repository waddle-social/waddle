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

export function isAiThreadPromptBody(body: string): boolean {
  return /^\s*\/ai(?:\s|$)/i.test(body) || /@waddle\b/i.test(body);
}

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

function isAiThreadRootCandidate(message: TimelineMessage, promptBody: string): boolean {
  return message.isSelf
    && message.body === promptBody
    && !message.replyTo
    && message.deliveryStatus === "delivered"
    && (!message.threadId || message.threadId === message.id);
}

export function nextAiThreadRootToOpen(
  messages: readonly TimelineMessage[],
  pendingPromptBodies: readonly string[],
  seenMessageIds: ReadonlySet<string>,
): { messageId: string; promptIndex: number } | undefined {
  for (const message of messages) {
    if (seenMessageIds.has(message.id)) continue;
    const promptIndex = pendingPromptBodies.findIndex((body) => isAiThreadRootCandidate(message, body));
    if (promptIndex >= 0) return { messageId: message.id, promptIndex };
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
