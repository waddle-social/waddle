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
      return `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="https://example.com/a?x=1&amp;y=2" normalized-url="https://example.com/a" expires-at="2999-01-01T00:00:00.000Z"><title>Example</title><description>Plain text</description></preview></lookup></iq>`;
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
      originalUrl: "https://example.com/a?x=1&y=2",
      normalizedUrl: "https://example.com/a",
      status: "ready",
      expiresAt: "2999-01-01T00:00:00.000Z",
      title: "Example",
      description: "Plain text",
    });
  });

  test("rejects lookup responses with non-HTTPS preview URLs", async () => {
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="http://example.com/a" normalized-url="http://example.com/a" expires-at="2999-01-01T00:00:00.000Z"><title>Example</title></preview></lookup></iq>`;

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

  test("rejects ready lookup responses for a different original URL than requested", async () => {
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="https://other.example/a" normalized-url="https://other.example/a" expires-at="2999-01-01T00:00:00.000Z"><title>Other</title></preview></lookup></iq>`;

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

  test("accepts ready lookup responses when server normalizes hostname case and implicit path", async () => {
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="https://example.com/" normalized-url="https://example.com/" expires-at="2999-01-01T00:00:00.000Z"><title>Example</title></preview></lookup></iq>`;

    let result: Awaited<ReturnType<typeof requestPlaintextLinkPreviewLookup>> = null;
    await withFakeXmlDocument(async () => {
      await withFakeDomParser(async () => {
        result = await requestPlaintextLinkPreviewLookup(
          { send_raw_iq },
          "read https://Example.COM",
          "room@muc.example.com",
        );
      });
    });

    expect(result).toMatchObject({
      token: "signed-token",
      originalUrl: "https://example.com/",
      normalizedUrl: "https://example.com/",
      status: "ready",
    });
  });

  test("accepts ready lookup responses when server removes default HTTPS port", async () => {
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="https://example.com/path" normalized-url="https://example.com/path" expires-at="2999-01-01T00:00:00.000Z"><title>Example</title></preview></lookup></iq>`;

    let result: Awaited<ReturnType<typeof requestPlaintextLinkPreviewLookup>> = null;
    await withFakeXmlDocument(async () => {
      await withFakeDomParser(async () => {
        result = await requestPlaintextLinkPreviewLookup(
          { send_raw_iq },
          "read https://example.com:443/path",
          "room@muc.example.com",
        );
      });
    });

    expect(result).toMatchObject({
      token: "signed-token",
      originalUrl: "https://example.com/path",
      normalizedUrl: "https://example.com/path",
      status: "ready",
    });
  });

  test("returns unsupported lookup state for not_found responses", async () => {
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="not_found"/></iq>`;

    let result: Awaited<ReturnType<typeof requestPlaintextLinkPreviewLookup>> = null;
    await withFakeXmlDocument(async () => {
      await withFakeDomParser(async () => {
        result = await requestPlaintextLinkPreviewLookup(
          { send_raw_iq },
          "read https://unsupported.example/a",
          "room@muc.example.com",
        );
      });
    });

    expect(result).toEqual({
      status: "not_found",
      originalUrl: "https://unsupported.example/a",
    });
  });

  test("returns typed normal lookup states for resolver blocked, failed, and unsupported responses", async () => {
    for (const status of ["blocked", "failed", "unsupported"] as const) {
      const send_raw_iq = async () =>
        `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="${status}"/></iq>`;

      let result: Awaited<ReturnType<typeof requestPlaintextLinkPreviewLookup>> = null;
      await withFakeXmlDocument(async () => {
        await withFakeDomParser(async () => {
          result = await requestPlaintextLinkPreviewLookup(
            { send_raw_iq },
            "read https://unsupported.example/a",
            "room@muc.example.com",
          );
        });
      });

      expect(result).toEqual({
        status,
        originalUrl: "https://unsupported.example/a",
      });
    }
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
