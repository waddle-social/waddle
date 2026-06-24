import { describe, expect, test } from "bun:test";

import { PresenceRegistry } from "../src/presence/presence-registry";

describe("PresenceRegistry", () => {
  test("a single available resource renders available", () => {
    const reg = new PresenceRegistry();
    expect(reg.apply("alice@waddle.test", "laptop", "available")).toBe("available");
    expect(reg.renderedFor("alice@waddle.test")).toBe("available");
  });

  test("a contact with no known resources renders offline", () => {
    const reg = new PresenceRegistry();
    expect(reg.renderedFor("ghost@waddle.test")).toBe("offline");
  });

  test("do-not-disturb on one device wins over available on another", () => {
    const reg = new PresenceRegistry();
    reg.apply("alice@waddle.test", "laptop", "available");
    expect(reg.apply("alice@waddle.test", "phone", "dnd")).toBe("dnd");
  });

  test("a resource going offline stops counting; the rest still shows", () => {
    const reg = new PresenceRegistry();
    reg.apply("alice@waddle.test", "laptop", "away");
    reg.apply("alice@waddle.test", "phone", "available");
    expect(reg.apply("alice@waddle.test", "phone", "offline")).toBe("away");
  });

  test("every resource offline renders the contact offline", () => {
    const reg = new PresenceRegistry();
    reg.apply("alice@waddle.test", "laptop", "available");
    expect(reg.apply("alice@waddle.test", "laptop", "offline")).toBe("offline");
  });

  test("a fresh Show for the same resource replaces the prior one", () => {
    const reg = new PresenceRegistry();
    reg.apply("alice@waddle.test", "laptop", "available");
    expect(reg.apply("alice@waddle.test", "laptop", "dnd")).toBe("dnd");
  });

  test("clear forgets all tracked presence", () => {
    const reg = new PresenceRegistry();
    reg.apply("alice@waddle.test", "laptop", "available");
    reg.apply("bob@waddle.test", "phone", "dnd");
    reg.clear();
    expect(reg.renderedFor("alice@waddle.test")).toBe("offline");
    expect(reg.renderedFor("bob@waddle.test")).toBe("offline");
  });
});
