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

  test("an away resource exposes its XEP-0319 idle instant", () => {
    const reg = new PresenceRegistry();
    reg.apply("alice@waddle.test", "laptop", "away", 1_000);
    expect(reg.renderedIdleFor("alice@waddle.test")).toBe(1_000);
  });

  test("an available contact reports no idle", () => {
    const reg = new PresenceRegistry();
    reg.apply("alice@waddle.test", "laptop", "available", 1_000);
    // Idle is only meaningful behind an away/xa dot; available reads as "here now".
    expect(reg.renderedIdleFor("alice@waddle.test")).toBeUndefined();
  });

  test("idle is forgotten once the away resource goes offline", () => {
    const reg = new PresenceRegistry();
    reg.apply("alice@waddle.test", "laptop", "away", 1_000);
    reg.apply("alice@waddle.test", "laptop", "offline");
    expect(reg.renderedIdleFor("alice@waddle.test")).toBeUndefined();
  });

  test("with two away devices, the most-recently-active idle is shown", () => {
    const reg = new PresenceRegistry();
    reg.apply("alice@waddle.test", "laptop", "away", 1_000);
    reg.apply("alice@waddle.test", "phone", "away", 5_000);
    // 5_000 is the later instant → the shorter idle → the one to render.
    expect(reg.renderedIdleFor("alice@waddle.test")).toBe(5_000);
  });
});
