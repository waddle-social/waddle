import { describe, expect, test } from "bun:test";
import {
  firstEligibleHttpsUrl,
  isTrustedCachedPreviewImageUrl,
  linkPreviewMediaOriginFromWebSocketUrl,
  requestPlaintextLinkPreviewLookup,
  trustedLinkPreviewMediaOrigin,
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

  test("accepts cached Waddle preview image metadata from ready lookup responses", async () => {
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="https://example.com/a" normalized-url="https://example.com/a" expires-at="2999-01-01T00:00:00.000Z"><title>Example</title><description>Plain text</description><image url="https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png" media-type="image/png" width="640" height="360" alt="Article screenshot"/></preview></lookup></iq>`;

    let result: Awaited<ReturnType<typeof requestPlaintextLinkPreviewLookup>> = null;
    await withFakeXmlDocument(async () => {
      await withFakeDomParser(async () => {
        result = await requestPlaintextLinkPreviewLookup(
          { send_raw_iq },
          "read https://example.com/a",
          "room@muc.example.com",
          "https://waddle.example",
        );
      });
    });

    expect(result).toMatchObject({
      status: "ready",
      image: {
        url: "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
        mediaType: "image/png",
        width: 640,
        height: 360,
        alt: "Article screenshot",
      },
    });
  });

  test("accepts cached preview images for trusted loopback HTTP development sessions", () => {
    const origin = linkPreviewMediaOriginFromWebSocketUrl("ws://localhost:4321/xmpp");

    expect(origin).toBe("http://localhost:4321");
    expect(isTrustedCachedPreviewImageUrl(
      "http://localhost:4321/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
      origin,
    )).toBe(true);
  });

  test("prefers explicit session media origin over websocket origin", () => {
    expect(trustedLinkPreviewMediaOrigin({
      xmpp_websocket_url: "wss://xmpp.example/ws",
      link_preview_media_origin: "https://cdn.example",
    })).toBe("https://cdn.example");
  });

  test("accepts trusted cached preview image origins with explicit default ports", () => {
    expect(isTrustedCachedPreviewImageUrl(
      "https://waddle.example:443/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
      "https://waddle.example",
    )).toBe(true);
    expect(isTrustedCachedPreviewImageUrl(
      "http://localhost:80/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
      "http://localhost",
    )).toBe(true);
  });

  test("rejects non-loopback HTTP cached preview image origins", () => {
    expect(isTrustedCachedPreviewImageUrl(
      "http://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
      "http://waddle.example",
    )).toBe(false);
  });

  test("drops lookup preview images from a foreign cached-media origin", async () => {
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="https://example.com/a" normalized-url="https://example.com/a" expires-at="2999-01-01T00:00:00.000Z"><title>Example</title><image url="https://attacker.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png" media-type="image/png"/></preview></lookup></iq>`;

    let result: Awaited<ReturnType<typeof requestPlaintextLinkPreviewLookup>> = null;
    await withFakeXmlDocument(async () => {
      await withFakeDomParser(async () => {
        result = await requestPlaintextLinkPreviewLookup(
          { send_raw_iq },
          "read https://example.com/a",
          "room@muc.example.com",
          "https://waddle.example",
        );
      });
    });

    expect(result).toMatchObject({ status: "ready" });
    expect(result && "image" in result ? result.image : undefined).toBeUndefined();
  });

  test("drops lookup preview images with unsafe media types", async () => {
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="https://example.com/a" normalized-url="https://example.com/a" expires-at="2999-01-01T00:00:00.000Z"><title>Example</title><image url="https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png" media-type="image/svg+xml"/></preview></lookup></iq>`;

    let result: Awaited<ReturnType<typeof requestPlaintextLinkPreviewLookup>> = null;
    await withFakeXmlDocument(async () => {
      await withFakeDomParser(async () => {
        result = await requestPlaintextLinkPreviewLookup(
          { send_raw_iq },
          "read https://example.com/a",
          "room@muc.example.com",
          "https://waddle.example",
        );
      });
    });

    expect(result).toMatchObject({ status: "ready" });
    expect(result && "image" in result ? result.image : undefined).toBeUndefined();
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

  test("rejects lookup responses with oversized preview tokens", async () => {
    const oversizedToken = "x".repeat(4097);
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="${oversizedToken}" original-url="https://example.com/a" normalized-url="https://example.com/a" expires-at="2999-01-01T00:00:00.000Z"><title>Example</title></preview></lookup></iq>`;

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

  test("rejects legacy not_found lookup responses", async () => {
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

    expect(result).toBeNull();
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

  test("parses player embed from ready lookup responses", async () => {
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="https://example.com/a" normalized-url="https://example.com/a" expires-at="2999-01-01T00:00:00.000Z"><title>Example</title><player xmlns="urn:waddle:link-preview:0" url="https://www.youtube-nocookie.com/embed/429A_VugWW0" width="1280" height="720"/></preview></lookup></iq>`;

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

    expect(result).toMatchObject({
      status: "ready",
      playerEmbed: {
        url: "https://www.youtube-nocookie.com/embed/429A_VugWW0",
        width: 1280,
        height: 720,
      },
    });
  });

  test("rejects player embed from a non-allowlisted origin in lookup responses", async () => {
    const send_raw_iq = async () =>
      `<iq type="result" id="lookup-1"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="https://example.com/a" normalized-url="https://example.com/a" expires-at="2999-01-01T00:00:00.000Z"><title>Example</title><player xmlns="urn:waddle:link-preview:0" url="https://attacker.example/embed/xss" width="1280" height="720"/></preview></lookup></iq>`;

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

    expect(result).toMatchObject({ status: "ready" });
    expect(result && "playerEmbed" in result ? result.playerEmbed : undefined).toBeUndefined();
  });

  test("accepts a lookup response just before the eight-second deadline", async () => {
    const clock = installLookupClock();
    try {
      await withFakeXmlDocument(async () => {
        await withFakeDomParser(async () => {
          let resolveResponse!: (response: string) => void;
          const response = new Promise<string>((resolve) => { resolveResponse = resolve; });
          const cancelled: string[] = [];
          const lookup = requestPlaintextLinkPreviewLookup(
            { send_raw_iq: () => response, cancel_raw_iq: (id) => { cancelled.push(id); } },
            "https://example.com/a",
            "room@muc.example.com",
          );
          clock.advance(7999);
          resolveResponse(`<iq type="result"><lookup xmlns="urn:waddle:link-preview:0" status="ready"><preview token="signed-token" original-url="https://example.com/a" normalized-url="https://example.com/a" expires-at="2999-01-01T00:00:00.000Z"><title>Example</title></preview></lookup></iq>`);
          expect(await lookup).toMatchObject({ status: "ready", title: "Example" });
          clock.advance(8000);
          expect(cancelled).toEqual([]);
          expect(clock.pendingCount()).toBe(0);
        });
      });
    } finally {
      clock.restore();
    }
  });

  test("expires an unanswered lookup at eight seconds and cancels its IQ", async () => {
    const clock = installLookupClock();
    try {
      await withFakeXmlDocument(async () => {
        let sentIq = "";
        const cancelled: string[] = [];
        let settled = false;
        const lookup = requestPlaintextLinkPreviewLookup(
          {
            send_raw_iq: (xml) => { sentIq = xml; return new Promise<string>(() => {}); },
            cancel_raw_iq: (id) => { cancelled.push(id); },
          },
          "https://example.com/a",
          "room@muc.example.com",
        ).then((result) => { settled = true; return result; });
        clock.advance(7999);
        await Promise.resolve();
        expect(settled).toBe(false);
        expect(cancelled).toEqual([]);
        clock.advance(1);
        expect(await lookup).toBeNull();
        expect(cancelled).toHaveLength(1);
        expect(sentIq).toContain(`id="${cancelled[0]}"`);
        expect(clock.pendingCount()).toBe(0);
      });
    } finally {
      clock.restore();
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

function installLookupClock() {
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  const pending = new Map<number, { at: number; callback: () => void }>();
  let now = 0;
  let nextId = 1;
  globalThis.setTimeout = ((handler: (...args: unknown[]) => void, timeout = 0, ...args: unknown[]) => {
    const id = nextId++;
    pending.set(id, { at: now + timeout, callback: () => handler(...args) });
    return id as unknown as ReturnType<typeof setTimeout>;
  }) as typeof setTimeout;
  globalThis.clearTimeout = ((id?: Parameters<typeof clearTimeout>[0]) => {
    pending.delete(id as unknown as number);
  }) as typeof clearTimeout;
  return {
    advance(milliseconds: number) {
      now += milliseconds;
      for (const [id, timer] of pending) {
        if (timer.at <= now) {
          pending.delete(id);
          timer.callback();
        }
      }
    },
    pendingCount: () => pending.size,
    restore() {
      globalThis.setTimeout = originalSetTimeout;
      globalThis.clearTimeout = originalClearTimeout;
    },
  };
}
