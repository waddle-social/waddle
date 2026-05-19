import { describe, expect, test } from "bun:test";
import { createCallVideoRegistry } from "../src/lib/calls/video-registry";

/**
 * The registry is the seam CallTileGrid uses to expose its `<video>`
 * elements to CallOverlay's PIP toggle. The contract that matters:
 *  - register(identity, el) stashes the element.
 *  - register(identity, null) clears the entry (Vue's `:ref` unmount
 *    signal) so a stale element from a previous tile mount isn't
 *    returned after the participant left.
 *  - register(identity, newEl) overwrites in place — a participant
 *    swapping camera off/on doesn't leak the previous element.
 */
describe("call-video-registry", () => {
  test("register stores an element retrievable by identity", () => {
    const reg = createCallVideoRegistry();
    const el = {} as HTMLVideoElement;
    reg.register("alice@waddle.test/web", el);
    expect(reg.get("alice@waddle.test/web")).toBe(el);
  });

  test("register(identity, null) clears the slot", () => {
    const reg = createCallVideoRegistry();
    const el = {} as HTMLVideoElement;
    reg.register("alice@waddle.test/web", el);
    reg.register("alice@waddle.test/web", null);
    expect(reg.get("alice@waddle.test/web")).toBeNull();
  });

  test("register with a fresh element overwrites the previous one", () => {
    const reg = createCallVideoRegistry();
    const first = { id: "1" } as unknown as HTMLVideoElement;
    const second = { id: "2" } as unknown as HTMLVideoElement;
    reg.register("alice@waddle.test/web", first);
    reg.register("alice@waddle.test/web", second);
    expect(reg.get("alice@waddle.test/web")).toBe(second);
  });

  test("entries() iterates only currently-registered (identity, el) pairs", () => {
    const reg = createCallVideoRegistry();
    const aliceEl = { id: "a" } as unknown as HTMLVideoElement;
    const bobEl = { id: "b" } as unknown as HTMLVideoElement;
    reg.register("alice@waddle.test/web", aliceEl);
    reg.register("bob@waddle.test/web", bobEl);
    reg.register("bob@waddle.test/web", null);
    const pairs = Array.from(reg.entries());
    expect(pairs).toEqual([["alice@waddle.test/web", aliceEl]]);
  });

  test("get for an unknown identity returns null (not undefined)", () => {
    const reg = createCallVideoRegistry();
    expect(reg.get("nobody@waddle.test")).toBeNull();
  });
});
