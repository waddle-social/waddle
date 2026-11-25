/**
 * #1256: the `mucPm` send option must cross the TS→WASM boundary as
 * `muc_pm` so the Rust builder appends the XEP-0045 §7.5
 * `<x xmlns='http://jabber.org/protocol/muc#user'/>` marker (asserted
 * wire-side in the Rust XEP-0045 suite).
 */
import { describe, expect, test } from "bun:test";
import { buildWasmSendOptions } from "../src/lib/xmpp/wasm-message-codecs";
import type { SendDirectMessageOptions } from "../src/lib/xmpp/send-types";

describe("buildWasmSendOptions MUC-PM marker", () => {
  test("maps mucPm to muc_pm for occupant-addressed sends", () => {
    const opts: SendDirectMessageOptions = { id: "m1", mucPm: true };
    expect(buildWasmSendOptions(opts, 0).muc_pm).toBe(true);
  });

  test("omits muc_pm for normal DM sends", () => {
    const opts: SendDirectMessageOptions = { id: "m2" };
    expect(buildWasmSendOptions(opts, 0).muc_pm).toBeUndefined();
  });
});

describe("buildWasmSendOptions XEP-0085 active", () => {
  test("maps chatStateActive to chat_state_active", () => {
    expect(buildWasmSendOptions({ id: "m3", chatStateActive: true }, 0).chat_state_active).toBe(true);
  });

  test("omits chat_state_active by default", () => {
    expect(buildWasmSendOptions({ id: "m4" }, 0).chat_state_active).toBeUndefined();
  });
});
