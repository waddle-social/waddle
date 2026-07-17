import { describe, expect, test } from "bun:test";
import {
  validatedWasmClientBinding,
  validatedWasmConfigBinding,
} from "../src/lib/xmpp/wasm-client-adapter";

type GeneratedClient = Parameters<typeof validatedWasmClientBinding>[0];
type GeneratedConfig = Parameters<typeof validatedWasmConfigBinding>[0];

const requiredMethods = [
  "connect",
  "disconnect",
  "dispose",
  "get_resume_state",
  "request_stream_management_ack",
  "send_chat_message",
  "send_groupchat_message",
  "set_on_call",
  "set_on_connected",
  "set_on_disconnected",
  "set_on_error",
  "set_on_mds_displayed",
  "set_on_message",
  "set_on_message_delivery_acked",
  "set_on_message_delivery_failed",
  "set_on_presence",
  "set_on_pubsub_event",
  "set_on_session_lifecycle",
  "set_on_stream_management",
] as const;

function generatedClientDouble(): Record<string, unknown> {
  return Object.fromEntries(
    requiredMethods.map((method) => [method, () => undefined]),
  );
}

describe("generated WASM client binding validation", () => {
  test("returns the exact generated instance after validating the strict surface", () => {
    const client = generatedClientDouble();

    expect(
      validatedWasmClientBinding(client as unknown as GeneratedClient),
    ).toBe(client);
  });

  test("fails closed with the missing generated method before activation", () => {
    const client = generatedClientDouble();
    Reflect.deleteProperty(client, "set_on_stream_management");

    expect(() =>
      validatedWasmClientBinding(client as unknown as GeneratedClient),
    ).toThrow(
      "Generated XMPP WASM binding is missing required method set_on_stream_management",
    );
  });

  test("rejects non-callable generated members", () => {
    const client = generatedClientDouble();
    client.send_chat_message = null;

    expect(() =>
      validatedWasmClientBinding(client as unknown as GeneratedClient),
    ).toThrow(
      "Generated XMPP WASM binding is missing required method send_chat_message",
    );
  });
});

describe("generated WASM config binding validation", () => {
  test("returns the exact config with the canonical snapshot method", () => {
    const config = { with_resume_state: () => undefined };
    expect(validatedWasmConfigBinding(config as unknown as GeneratedConfig))
      .toBe(config);
  });

  test("fails closed when the canonical snapshot method is missing", () => {
    expect(() => validatedWasmConfigBinding({} as GeneratedConfig)).toThrow(
      "Generated XMPP WASM config is missing required method with_resume_state",
    );
  });

  test("rejects a non-callable canonical snapshot member", () => {
    const config = { with_resume_state: null };
    expect(() => validatedWasmConfigBinding(config as unknown as GeneratedConfig))
      .toThrow(
        "Generated XMPP WASM config is missing required method with_resume_state",
      );
  });
});
