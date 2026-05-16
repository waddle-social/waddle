import { describe, expect, test } from "bun:test";
import {
  findMessageById,
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

  test("keeps the first owner for conflicting alias index entries", () => {
    const a: Message = { id: "a", wireIds: ["origin"], body: "first" };
    const b: Message = { id: "b", wireIds: ["origin"], body: "second" };
    const index = new Map<string, Message>();
    indexMessageByIds(index, a);
    indexMessageByIds(index, b);
    expect(index.get("origin")?.id).toBe("a");
    expect(index.get("a")?.id).toBe("a");
    expect(index.get("b")?.id).toBe("b");
  });
});
