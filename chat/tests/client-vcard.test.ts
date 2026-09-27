/**
 * Unit tests for the vCard/profile module extracted from
 * `BrowserXmppClient` (`src/lib/xmpp/client-vcard.ts`): XEP-0292
 * vCard4 field mapping in both directions and avatar URL/data
 * resolution — all against a fake WASM client.
 */
import { describe, expect, test } from "bun:test";
import { VCardManager, type VCardWasmClient } from "../src/lib/xmpp/client-vcard";
import type { WasmVCard4 } from "../src/lib/xmpp/wasm-types";

function createManager(xmpp: VCardWasmClient) {
  return new VCardManager({ requireConnectedXmpp: async () => xmpp });
}

describe("VCardManager", () => {
  test("fetchVCard4 maps snake_case wire fields into the camelCase profile", async () => {
    const manager = createManager({
      fetch_vcard4: async () => ({
        fn: "Alice Example",
        nickname: "alice",
        pronouns: "she/her",
        note: "hello",
        url: "https://alice.example",
        photo_uri: "https://alice.example/a.png",
      }),
    });

    expect(await manager.fetchVCard4("alice@example.com")).toEqual({
      fullName: "Alice Example",
      nickname: "alice",
      pronouns: "she/her",
      note: "hello",
      url: "https://alice.example",
      photoUri: "https://alice.example/a.png",
    });
  });

  test("fetchVCard4 returns null when no vCard is published", async () => {
    const manager = createManager({ fetch_vcard4: async () => null });
    expect(await manager.fetchVCard4("bob@example.com")).toBeNull();
  });

  test("publishVCard4 only serialises the fields that are set", async () => {
    const published: WasmVCard4[] = [];
    const manager = createManager({
      publish_vcard4: async (vcard) => {
        published.push(vcard);
        return undefined;
      },
    });

    await manager.publishVCard4({ nickname: "alice", note: "hi" });

    expect(published).toEqual([{ nickname: "alice", note: "hi" }]);
  });

  test("fetchUserAvatar returns in-band data as a data URL and resolves the bare JID", async () => {
    const requested: string[] = [];
    const manager = createManager({
      request_avatar: async (jid) => {
        requested.push(jid);
        return { jid, id: "a2", mime_type: "image/png", data: new Uint8Array([1, 2, 3]) };
      },
    });
    expect(await manager.fetchUserAvatar("bob@example.com/phone")).toStartWith("data:image/png;base64,");
    expect(requested).toEqual(["bob@example.com"]);

    const metadataOnly = createManager({
      request_avatar: async (jid) => ({ jid, id: "a3", mime_type: "image/png" }),
    });
    expect(await metadataOnly.fetchUserAvatar("bob@example.com")).toBeNull();

    const none = createManager({});
    expect(await none.fetchUserAvatar("bob@example.com")).toBeNull();
  });
});
