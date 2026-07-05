import { describe, expect, test } from "bun:test";
import { safeAttachmentUrl } from "../src/lib/attachment-url";

describe("safeAttachmentUrl", () => {
  test("allows https URLs", () => {
    expect(safeAttachmentUrl("https://files.example.com/a/report.pdf"))
      .toBe("https://files.example.com/a/report.pdf");
  });

  test("allows http URLs", () => {
    expect(safeAttachmentUrl("http://files.example.com/pic.png"))
      .toBe("http://files.example.com/pic.png");
  });

  test("allows locally-minted blob URLs (decrypted attachments)", () => {
    expect(safeAttachmentUrl("blob:https://waddle.chat/6f2c1a2e-1111-4111-8111-aaaaaaaaaaaa"))
      .toBe("blob:https://waddle.chat/6f2c1a2e-1111-4111-8111-aaaaaaaaaaaa");
  });

  test("trims surrounding whitespace from an allowed URL", () => {
    expect(safeAttachmentUrl("  https://files.example.com/a.png \n"))
      .toBe("https://files.example.com/a.png");
  });

  test("rejects javascript: URLs", () => {
    expect(safeAttachmentUrl("javascript:alert(1)")).toBeNull();
  });

  test("rejects javascript: URLs regardless of case or leading whitespace", () => {
    expect(safeAttachmentUrl(" JaVaScRiPt:alert(1)")).toBeNull();
  });

  test("rejects data: URLs", () => {
    expect(safeAttachmentUrl("data:text/html,<script>alert(1)</script>")).toBeNull();
  });

  test("rejects vbscript: URLs", () => {
    expect(safeAttachmentUrl("vbscript:msgbox(1)")).toBeNull();
  });

  test("rejects file: URLs", () => {
    expect(safeAttachmentUrl("file:///etc/passwd")).toBeNull();
  });

  test("rejects unparseable values", () => {
    expect(safeAttachmentUrl("not a url")).toBeNull();
  });

  test("rejects null, undefined, and empty values", () => {
    expect(safeAttachmentUrl(null)).toBeNull();
    expect(safeAttachmentUrl(undefined)).toBeNull();
    expect(safeAttachmentUrl("")).toBeNull();
    expect(safeAttachmentUrl("   ")).toBeNull();
  });
});
