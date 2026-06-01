import { describe, expect, test } from "bun:test";
import {
  firstEligibleHttpsUrl,
  requestPlaintextLinkPreviewLookup,
} from "../src/lib/xmpp/link-preview";
import { withFakeDomParser, withFakeXmlDocument } from "./helpers/disco-xml";

describe("link preview lookup", () => {
  test("selects the first eligible HTTPS URL in body order", () => {
    expect(firstEligibleHttpsUrl("see http://ignored.example then https://first.example/a, and https://second.example/b"))
      .toBe("https://first.example/a");
  });

  test("selects HTTPS URL wrapped in punctuation", () => {
    expect(firstEligibleHttpsUrl("see (https://example.com/a)."))
      .toBe("https://example.com/a");
  });

  test("selects HTTPS URL after inline punctuation", () => {
    expect(firstEligibleHttpsUrl("see:https://example.com/a then https://second.example/a"))
      .toBe("https://example.com/a");
  });

  test("requests XMPP-native composer lookup with URL and scope", async () => {
    const send_raw_iq = async (xml: string) => {
      expect(xml).toContain("<url>https://example.com/a?x=1&amp;y=2</url>");
      expect(xml).toContain("<scope>room@muc.example.com</scope>");
      return `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="https://example.com/a" normalized-url="https://example.com/a" expires-at="2026-06-01T12:05:00.000Z"><title>Example</title><description>Plain text</description></preview></lookup></iq>`;
    };

    let result: Awaited<ReturnType<typeof requestPlaintextLinkPreviewLookup>> = null;
    await withFakeXmlDocument(async () => {
      await withFakeDomParser(async () => {
        result = await requestPlaintextLinkPreviewLookup(
          { send_raw_iq },
          "read https://example.com/a?x=1&y=2",
          "room@muc.example.com",
        );
      });
    });

    expect(result).toEqual({
      token: "signed-token",
      originalUrl: "https://example.com/a",
      normalizedUrl: "https://example.com/a",
      status: "ready",
      expiresAt: "2026-06-01T12:05:00.000Z",
      title: "Example",
      description: "Plain text",
    });
  });

  test("rejects lookup responses with non-HTTPS preview URLs", async () => {
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="http://example.com/a" normalized-url="http://example.com/a" expires-at="2026-06-01T12:05:00.000Z"><title>Example</title></preview></lookup></iq>`;

    let result: Awaited<ReturnType<typeof requestPlaintextLinkPreviewLookup>> = null;
    await withFakeXmlDocument(async () => {
      await withFakeDomParser(async () => {
        result = await requestPlaintextLinkPreviewLookup(
          { send_raw_iq },
          "read https://example.com/a",
          "room@muc.example.com",
        );
      });
    });

    expect(result).toBeNull();
  });

  test("lookup failure is a metadata miss instead of a send blocker", async () => {
    const result = await requestPlaintextLinkPreviewLookup(
      { send_raw_iq: async () => { throw new Error("lookup failed"); } },
      "read https://example.com/a",
      "room@muc.example.com",
    );

    expect(result).toBeNull();
  });
});
