import { afterEach, describe, expect, mock, test } from "bun:test";
import { findMessageElementById } from "../src/lib/message-targeting";

type StubElement = {
  getAttribute(name: string): string | null;
};

function makeElement(messageId: string): StubElement {
  return {
    getAttribute(name: string) {
      return name === "data-message-id" ? messageId : null;
    },
  };
}

const originalCssDescriptor = Object.getOwnPropertyDescriptor(globalThis, "CSS");

afterEach(() => {
  if (originalCssDescriptor) {
    Object.defineProperty(globalThis, "CSS", originalCssDescriptor);
    return;
  }

  delete (globalThis as { CSS?: unknown }).CSS;
});

describe("findMessageElementById", () => {
  test("uses CSS.escape before building the selector", () => {
    const dangerId = 'reply"] [data-message-id="other';
    const target = makeElement(dangerId);
    const escape = mock(() => "escaped-id");
    const querySelector = mock((selector: string) => {
      expect(selector).toBe('[data-message-id="escaped-id"]');
      return target;
    });
    const querySelectorAll = mock(() => []);

    Object.defineProperty(globalThis, "CSS", {
      value: { escape },
      configurable: true,
      writable: true,
    });

    const result = findMessageElementById({ querySelector, querySelectorAll }, dangerId);

    expect(result).toBe(target);
    expect(escape).toHaveBeenCalledWith(dangerId);
    expect(querySelectorAll).not.toHaveBeenCalled();
  });

  test("falls back to exact attribute matching when CSS.escape is unavailable", () => {
    const dangerId = 'reply"] [data-message-id="other';
    const wrong = makeElement("other");
    const target = makeElement(dangerId);
    const querySelector = mock(() => wrong);
    const querySelectorAll = mock(() => [wrong, target]);

    delete (globalThis as { CSS?: unknown }).CSS;

    const result = findMessageElementById({ querySelector, querySelectorAll }, dangerId);

    expect(result).toBe(target);
    expect(querySelector).not.toHaveBeenCalled();
    expect(querySelectorAll).toHaveBeenCalledWith("[data-message-id]");
  });

  test("rejects a mismatched selector result and scans for the exact message id", () => {
    const dangerId = 'reply"] [data-message-id="other';
    const wrong = makeElement("other");
    const target = makeElement(dangerId);
    const querySelector = mock(() => wrong);
    const querySelectorAll = mock(() => [wrong, target]);

    Object.defineProperty(globalThis, "CSS", {
      value: { escape: (value: string) => value },
      configurable: true,
      writable: true,
    });

    const result = findMessageElementById({ querySelector, querySelectorAll }, dangerId);

    expect(result).toBe(target);
    expect(querySelector).toHaveBeenCalledTimes(1);
    expect(querySelectorAll).toHaveBeenCalledWith("[data-message-id]");
  });
});
