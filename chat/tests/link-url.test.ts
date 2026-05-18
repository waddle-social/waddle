import { describe, expect, test } from "bun:test";
import { sanitizeLinkUrl } from "../src/lib/composer/link-url";

describe("sanitizeLinkUrl", () => {
  test("returns the canonical URL for http", () => {
    expect(sanitizeLinkUrl("http://example.com")).toBe("http://example.com/");
  });

  test("returns the canonical URL for https", () => {
    expect(sanitizeLinkUrl("https://example.com/path?q=1")).toBe("https://example.com/path?q=1");
  });

  test("trims surrounding whitespace before parsing", () => {
    expect(sanitizeLinkUrl("  https://example.com  ")).toBe("https://example.com/");
  });

  test("permits mailto: addresses", () => {
    expect(sanitizeLinkUrl("mailto:user@example.com")).toBe("mailto:user@example.com");
  });

  test("rejects javascript: URLs", () => {
    expect(sanitizeLinkUrl("javascript:alert(1)")).toBeNull();
  });

  test("rejects data: URLs", () => {
    expect(sanitizeLinkUrl("data:text/html,<script>alert(1)</script>")).toBeNull();
  });

  test("rejects file: URLs", () => {
    expect(sanitizeLinkUrl("file:///etc/passwd")).toBeNull();
  });

  test("returns null for empty input", () => {
    expect(sanitizeLinkUrl("")).toBeNull();
    expect(sanitizeLinkUrl("   ")).toBeNull();
  });

  test("returns null for bare hostnames without a scheme", () => {
    // sanitizeLinkUrl is strict — we expect the popover to either accept a
    // fully-qualified URL or treat the input as invalid. Implicit-protocol
    // handling belongs in the paste flow, not the link button.
    expect(sanitizeLinkUrl("example.com")).toBeNull();
  });
});
