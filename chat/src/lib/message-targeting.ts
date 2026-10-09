const DATA_MESSAGE_ID_ATTRIBUTE = "data-message-id";
const ALL_MESSAGE_SELECTOR = `[${DATA_MESSAGE_ID_ATTRIBUTE}]`;

type MessageTargetingElement = {
  getAttribute(name: string): string | null;
};

type MessageTargetingRoot<T extends MessageTargetingElement> = {
  querySelector(selector: string): T | null;
  querySelectorAll(selector: string): Iterable<T>;
};

function findMessageElement<T extends MessageTargetingElement>(
  root: MessageTargetingRoot<T> | null | undefined,
  messageId: string,
  attribute: "data-message-id" | "data-message-row-key",
): T | null {
  if (!root) return null;

  if (typeof CSS !== "undefined" && typeof CSS.escape === "function") {
    const candidate = root.querySelector(
      `[${attribute}="${CSS.escape(messageId)}"]`,
    );
    if (candidate?.getAttribute(attribute) === messageId) {
      return candidate;
    }
  }

  for (const candidate of root.querySelectorAll(attribute === DATA_MESSAGE_ID_ATTRIBUTE ? ALL_MESSAGE_SELECTOR : `[${attribute}]`)) {
    if (candidate?.getAttribute(attribute) === messageId) {
      return candidate;
    }
  }

  return null;
}

export function findMessageElementById<T extends MessageTargetingElement>(
  root: MessageTargetingRoot<T> | null | undefined,
  messageId: string,
): T | null {
  return findMessageElement(root, messageId, DATA_MESSAGE_ID_ATTRIBUTE);
}

/** Local presentation anchors never depend on colliding room wire IDs. */
export function findMessageElementByRowKey<T extends MessageTargetingElement>(
  root: MessageTargetingRoot<T> | null | undefined,
  rowKey: string,
): T | null {
  return findMessageElement(root, rowKey, "data-message-row-key");
}
