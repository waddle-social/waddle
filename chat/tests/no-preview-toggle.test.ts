import { describe, expect, test } from "bun:test";
import { createClient } from "stanza";
import { registerWaddleExtensions } from "../src/lib/xmpp/extensions";

/**
 * Verifies that setting `message.suppressLinkPreview = {}` on an
 * outgoing message serializes to a top-level
 * `<no-preview xmlns='urn:waddle:link-preview:0'/>` child — the
 * sender-side signal the server's link-preview enricher honors to
 * skip enrichment for a single message.
 */
describe("<no-preview/> suppression marker on outbound", () => {
  test("serializes to a top-level no-preview element", () => {
    const client = createClient({ jid: "alice@localhost" });
    registerWaddleExtensions(client);

    const xml = client.stanzas.export("message", {
      id: "m1",
      to: "room@muc.localhost",
      type: "groupchat",
      body: "secret https://example.com/x",
      // Presence-only alias set on the message to emit the hint.
      suppressLinkPreview: {},
    } as unknown as Record<string, unknown>);

    const serialized = xml?.toString() ?? "";
    expect(serialized).toContain("urn:waddle:link-preview:0");
    expect(serialized).toContain("no-preview");
  });

  test("no marker when suppressLinkPreview omitted", () => {
    const client = createClient({ jid: "alice@localhost" });
    registerWaddleExtensions(client);

    const xml = client.stanzas.export("message", {
      id: "m1",
      to: "room@muc.localhost",
      type: "groupchat",
      body: "see https://example.com/x",
    } as unknown as Record<string, unknown>);

    const serialized = xml?.toString() ?? "";
    expect(serialized).not.toContain("no-preview");
  });
});
