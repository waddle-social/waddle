import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { TypedEventBus } from "../src/lib/xmpp/client-events";

type TestEvents = {
  message: [body: string];
  ack: [id: string, meta: { latencyMs: number }];
  disconnect: [];
};

function makeBus() {
  return new TypedEventBus<TestEvents>();
}

describe("TypedEventBus", () => {
  const spies: Array<{ mockRestore: () => void }> = [];
  afterEach(() => {
    for (const spy of spies.splice(0)) spy.mockRestore();
  });

  test("on/emit delivers typed payloads to a listener", () => {
    const bus = makeBus();
    const seen: Array<[string, number]> = [];
    bus.on("ack", (id, meta) => seen.push([id, meta.latencyMs]));
    bus.emit("ack", "m1", { latencyMs: 42 });
    expect(seen).toEqual([["m1", 42]]);
  });

  test("emit with no listeners is a no-op", () => {
    const bus = makeBus();
    expect(() => bus.emit("message", "hello")).not.toThrow();
    expect(() => bus.emit("disconnect")).not.toThrow();
  });

  test("on supports multiple listeners in registration order", () => {
    const bus = makeBus();
    const order: string[] = [];
    bus.on("message", (body) => order.push(`a:${body}`));
    bus.on("message", (body) => order.push(`b:${body}`));
    bus.emit("message", "x");
    expect(order).toEqual(["a:x", "b:x"]);
  });

  test("on returns an unsubscribe that removes only that listener", () => {
    const bus = makeBus();
    const seen: string[] = [];
    const offA = bus.on("message", (body) => seen.push(`a:${body}`));
    bus.on("message", (body) => seen.push(`b:${body}`));
    offA();
    bus.emit("message", "x");
    expect(seen).toEqual(["b:x"]);
    // Unsubscribing twice is harmless.
    expect(() => offA()).not.toThrow();
  });

  test("set replaces the previous single listener (setter semantics)", () => {
    const bus = makeBus();
    const seen: string[] = [];
    bus.set("message", (body) => seen.push(`first:${body}`));
    bus.set("message", (body) => seen.push(`second:${body}`));
    bus.emit("message", "x");
    expect(seen).toEqual(["second:x"]);
  });

  test("set(null) clears listeners (setMdsDisplayedHandler(null) semantics)", () => {
    const bus = makeBus();
    const seen: string[] = [];
    bus.set("message", (body) => seen.push(body));
    bus.set("message", null);
    bus.emit("message", "x");
    expect(seen).toEqual([]);
  });

  test("set clears listeners registered via on (setPubsubEventHandler semantics)", () => {
    const bus = makeBus();
    const seen: string[] = [];
    bus.on("message", (body) => seen.push(`on:${body}`));
    bus.set("message", (body) => seen.push(`set:${body}`));
    bus.emit("message", "x");
    expect(seen).toEqual(["set:x"]);
  });

  test("emit propagates listener errors (matches direct handler invocation)", () => {
    const bus = makeBus();
    bus.set("message", () => {
      throw new Error("boom");
    });
    expect(() => bus.emit("message", "x")).toThrow("boom");
  });

  test("emitSafe isolates listener errors so later hooks still run (fireHook semantics)", () => {
    const bus = makeBus();
    const consoleSpy = spyOn(console, "error").mockImplementation(() => {});
    spies.push(consoleSpy);
    const seen: string[] = [];
    bus.on("message", () => {
      throw new Error("boom");
    });
    bus.on("message", (body) => seen.push(body));
    expect(() => bus.emitSafe("message", "x")).not.toThrow();
    expect(seen).toEqual(["x"]);
    expect(consoleSpy).toHaveBeenCalledTimes(1);
    expect(consoleSpy.mock.calls[0]?.[0]).toBe("xmpp telemetry hook threw");
  });

  test("listener unsubscribed during emit of a later event no longer fires", () => {
    const bus = makeBus();
    const seen: string[] = [];
    const off = bus.on("message", (body) => seen.push(`a:${body}`));
    bus.on("message", (body) => {
      seen.push(`b:${body}`);
      off();
    });
    bus.emit("message", "1");
    bus.emit("message", "2");
    expect(seen).toEqual(["a:1", "b:1", "b:2"]);
  });

  test("events are independent per name", () => {
    const bus = makeBus();
    const seen: string[] = [];
    bus.on("message", (body) => seen.push(`msg:${body}`));
    bus.on("disconnect", () => seen.push("disc"));
    bus.emit("disconnect");
    expect(seen).toEqual(["disc"]);
  });
});
