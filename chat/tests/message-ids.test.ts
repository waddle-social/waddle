import { describe, expect, test } from "bun:test";
import {
  findMessageById,
  findMessageIndexById,
  indexMessageByIds,
} from "../src/lib/message-ids";

type Message = {
  id: string;
  wireIds?: string[];
  body: string;
};

describe("message id aliasing", () => {
  test("does not return an ambiguous shared wire-id alias", () => {
    const messages: Message[] = [
      { id: "a", wireIds: ["origin"], body: "first" },
      { id: "b", wireIds: ["origin"], body: "second" },
    ];
    expect(findMessageById(messages, "origin")).toBeUndefined();
    expect(findMessageById(messages, "a")?.body).toBe("first");
    expect(findMessageById(messages, "b")?.body).toBe("second");
  });

  test("findMessageIndexById mirrors collision-safety of findMessageById", () => {
    const messages: Message[] = [
      { id: "a", wireIds: ["origin"], body: "first" },
      { id: "b", wireIds: ["origin"], body: "second" },
    ];
    expect(findMessageIndexById(messages, "origin")).toBe(-1);
    expect(findMessageIndexById(messages, "a")).toBe(0);
    expect(findMessageIndexById(messages, "b")).toBe(1);
    expect(findMessageIndexById(messages, "missing")).toBe(-1);
    expect(findMessageIndexById(messages, null)).toBe(-1);
  });

  test("drops the alias entry when two distinct messages claim the same wireId", () => {
    const a: Message = { id: "a", wireIds: ["origin"], body: "first" };
    const b: Message = { id: "b", wireIds: ["origin"], body: "second" };
    const index = new Map<string, Message>();
    indexMessageByIds(index, a);
    indexMessageByIds(index, b);
    // Primary ids still resolve uniquely.
    expect(index.get("a")?.id).toBe("a");
    expect(index.get("b")?.id).toBe("b");
    // The conflicting alias resolves to neither, so callers can't merge or
    // update the wrong timeline item through it.
    expect(index.has("origin")).toBe(false);
  });

  test("re-indexing the same message under a shared alias does not drop it", () => {
    const a: Message = { id: "a", wireIds: ["origin"], body: "first" };
    const index = new Map<string, Message>();
    indexMessageByIds(index, a);
    indexMessageByIds(index, a);
    expect(index.get("origin")?.id).toBe("a");
  });
});

