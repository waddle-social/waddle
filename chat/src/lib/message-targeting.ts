const DATA_MESSAGE_ID_ATTRIBUTE = "data-message-id";
const ALL_MESSAGE_SELECTOR = `[${DATA_MESSAGE_ID_ATTRIBUTE}]`;

type MessageTargetingElement = {
  getAttribute(name: string): string | null;
};

type MessageTargetingRoot<T extends MessageTargetingElement> = {
  querySelector(selector: string): T | null;
  querySelectorAll(selector: string): Iterable<T>;
};

function hasExactMessageId<T extends MessageTargetingElement>(
  candidate: T | null | undefined,
  messageId: string,
): candidate is T {
  return candidate?.getAttribute(DATA_MESSAGE_ID_ATTRIBUTE) === messageId;
}

export function findMessageElementById<T extends MessageTargetingElement>(
  root: MessageTargetingRoot<T> | null | undefined,
  messageId: string,
): T | null {
  if (!root) return null;

  if (typeof CSS !== "undefined" && typeof CSS.escape === "function") {
    const candidate = root.querySelector(
      `[${DATA_MESSAGE_ID_ATTRIBUTE}="${CSS.escape(messageId)}"]`,
    );
    if (hasExactMessageId(candidate, messageId)) {
      return candidate;
    }
  }

  for (const candidate of root.querySelectorAll(ALL_MESSAGE_SELECTOR)) {
    if (hasExactMessageId(candidate, messageId)) {
      return candidate;
    }
  }

  return null;
}
