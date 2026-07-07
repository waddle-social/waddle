import { describe, expect, test } from "bun:test";
import {
  findMessageById,
  findMessageIndexById,
  MessageIdIndex,
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

  test("a later primary-id match wins over earlier alias ambiguity", () => {
    // Two rows share a reused origin-id alias; a third row's PRIMARY id is
    // the lookup candidate. The scan must reach the primary match instead
    // of short-circuiting to the ambiguity sentinel — otherwise a
    // redelivered copy of "c" (e.g. MAM catch-up after a server restart)
    // fails to reconcile and appends a duplicate row.
    const messages: Message[] = [
      { id: "a", wireIds: ["c"], body: "first-claimer" },
      { id: "b", wireIds: ["c"], body: "second-claimer" },
      { id: "c", body: "primary-owner" },
    ];
    expect(findMessageIndexById(messages, "c")).toBe(2);
    expect(findMessageById(messages, "c")?.body).toBe("primary-owner");
    // Without a primary owner the alias stays ambiguous.
    expect(findMessageIndexById(messages.slice(0, 2), "c")).toBe(-1);
  });

  test("findMessageIndexById predicate narrows candidates without losing collision safety", () => {
    const messages = [
      { id: "remote-1", wireIds: ["origin"], body: "remote", isSelf: false },
      { id: "self-1", wireIds: ["origin"], body: "self", isSelf: true },
    ] as const;
    // Without predicate: primary id wins so "remote-1" resolves.
    expect(findMessageIndexById(messages, "remote-1")).toBe(0);
    // With self predicate: remote row is invisible, self primary resolves.
    expect(findMessageIndexById(messages, "self-1", (m) => m.isSelf)).toBe(1);
    // Alias is ambiguous even when scoped only to self (no candidate matches).
    // After scoping to self, only self-1 claims "origin", so it resolves cleanly.
    expect(findMessageIndexById(messages, "origin", (m) => m.isSelf)).toBe(1);
    // Filtering out the self message hides the alias entirely.
    expect(findMessageIndexById(messages, "origin", (m) => !m.isSelf)).toBe(0);
  });
});

describe("MessageIdIndex", () => {
  test("drops the alias entry when two distinct messages claim the same wireId", () => {
    const a: Message = { id: "a", wireIds: ["origin"], body: "first" };
    const b: Message = { id: "b", wireIds: ["origin"], body: "second" };
    const index = new MessageIdIndex<Message>();
    index.add(a);
    index.add(b);
    // Primary ids still resolve uniquely.
    expect(index.get("a")?.id).toBe("a");
    expect(index.get("b")?.id).toBe("b");
    // The conflicting alias resolves to neither.
    expect(index.get("origin")).toBeUndefined();
  });

  test("re-indexing the same message under a shared alias does not drop it", () => {
    const a: Message = { id: "a", wireIds: ["origin"], body: "first" };
    const index = new MessageIdIndex<Message>();
    index.add(a);
    index.add(a);
    expect(index.get("origin")?.id).toBe("a");
  });

  test("alias colliding with another message's primary id preserves the primary lookup", () => {
    // messageA's canonical primary id is "origin"; messageB later lists
    // "origin" in wireIds. The primary lookup for "origin" must continue
    // to resolve to messageA — otherwise MAM merge/parent lookups miss
    // the canonical row by its own id.
    const a: Message = { id: "origin", body: "primary-owner" };
    const b: Message = { id: "b", wireIds: ["origin"], body: "alias-claimer" };
    const index = new MessageIdIndex<Message>();
    index.add(a);
    index.add(b);
    expect(index.get("origin")?.id).toBe("origin");
    expect(index.get("b")?.id).toBe("b");
  });

  test("tombstoned alias cannot be reclaimed by a third message", () => {
    // After a collision drops the alias, a later message with the same
    // wireId must not silently take ownership and re-introduce ambiguity.
    const a: Message = { id: "a", wireIds: ["origin"], body: "first" };
    const b: Message = { id: "b", wireIds: ["origin"], body: "second" };
    const c: Message = { id: "c", wireIds: ["origin"], body: "third" };
    const index = new MessageIdIndex<Message>();
    index.add(a);
    index.add(b);
    index.add(c);
    expect(index.get("origin")).toBeUndefined();
    expect(index.get("a")?.id).toBe("a");
    expect(index.get("b")?.id).toBe("b");
    expect(index.get("c")?.id).toBe("c");
  });

  test("blank/empty candidates do not resolve", () => {
    const a: Message = { id: "a", body: "x" };
    const index = new MessageIdIndex<Message>();
    index.add(a);
    expect(index.get("")).toBeUndefined();
    expect(index.get("  ")).toBeUndefined();
    expect(index.get(null)).toBeUndefined();
    expect(index.get(undefined)).toBeUndefined();
  });
});


