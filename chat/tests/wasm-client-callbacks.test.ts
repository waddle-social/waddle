import { describe, expect, test } from "bun:test";
import { WasmClientCallbackDouble } from "./helpers/wasm-client-callbacks";

describe("strict generated WASM callback test control", () => {
  test("keeps one canonical callback per generated setter", () => {
    const client = new WasmClientCallbackDouble();
    const received: string[] = [];
    client.set_on_message_delivery_acked((stanzaId) => {
      received.push(`old:${stanzaId}`);
    });
    client.set_on_message_delivery_acked((stanzaId) => {
      received.push(`current:${stanzaId}`);
    });

    client.emitMessageDeliveryAcked("message-1");

    expect(received).toEqual(["current:message-1"]);
  });

  test("rejects the removed object ACK alias", () => {
    const client = new WasmClientCallbackDouble();
    expect(() => client.emitMessageDeliveryAcked({ id: "message-1" } as never))
      .toThrow("ACK must be a stanza-id string");
  });

  test("exposes canonical fresh and resumed lifecycle values", () => {
    const client = new WasmClientCallbackDouble();
    const received: Array<"fresh" | "resumed"> = [];
    client.set_on_session_lifecycle((event) => received.push(event));

    client.emitSessionLifecycle("fresh");
    client.emitSessionLifecycle("resumed");

    expect(received).toEqual(["fresh", "resumed"]);
  });

  test("rejects removed lifecycle event aliases", () => {
    const client = new WasmClientCallbackDouble();
    expect(() => client.emitSessionLifecycle("session:started" as never))
      .toThrow("session lifecycle must be fresh or resumed");
  });
});
