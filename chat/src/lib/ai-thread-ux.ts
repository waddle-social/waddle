import type { TimelineMessage } from "@/lib/chat-ui";

const AI_CHATBOT_COMMAND_PATTERNS = [
  /(?:^|[:/#-])ai-chatbot(?:$|[:/#-])/i,
  /\bai\s+chatbot\b/i,
  /\bask\s+ai\b/i,
];

export function isAiThreadPromptBody(body: string): boolean {
  return /^\s*\/ai(?:\s|$)/i.test(body) || /@waddle\b/i.test(body);
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
