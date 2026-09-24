import { describe, expect, test } from "bun:test";
import { formatMePreview, parseMeAction } from "../src/lib/me-command";
import { renderMeActionHtml } from "../src/lib/me-action-render";

describe("parseMeAction (XEP-0245)", () => {
  test("matches the exact four-character /me prefix", () => {
    expect(parseMeAction("/me shrugs in disgust")).toEqual({ action: "shrugs in disgust" });
    expect(parseMeAction("/me ")).toEqual({ action: "" });
  });

  test("rejects the XEP-0245 non-matching examples", () => {
    for (const body of [
      "/meshrugs in disgust",
      "/me's disgusted",
      " /me shrugs in disgust",
      "\"/me shrugs in disgust\"",
      "* Atlas shrugs in disgust",
      "Why did Atlas say \"/me shrugs in disgust\"?",
      "/me",
      "/ME shrugs",
    ]) {
      expect(parseMeAction(body)).toBeNull();
    }
  });
});

describe("formatMePreview", () => {
  test("renders /me bodies as `* Sender action`", () => {
    expect(formatMePreview("/me waves", "Atlas")).toBe("* Atlas waves");
    expect(formatMePreview("/me ", "Atlas")).toBe("* Atlas");
  });

  test("leaves other bodies untouched", () => {
    expect(formatMePreview("hello /me waves", "Atlas")).toBe("hello /me waves");
  });
});

describe("renderMeActionHtml", () => {
  test("prefixes the actor and drops the /me command", () => {
    expect(renderMeActionHtml({ body: "/me waves", actor: "Atlas" }))
      .toBe('<p><span class="font-semibold">* Atlas</span> waves</p>');
  });

  test("escapes the actor and never mention-highlights it", () => {
    const html = renderMeActionHtml({ body: "/me waves", actor: "<b>@evil</b>" });
    expect(html).toContain("* &lt;b&gt;@evil&lt;/b&gt;");
    expect(html).not.toContain("rich-mention");
    expect(html).not.toContain("<b>");
  });

  test("keeps XEP-0394 markup offsets (code points into the full body) aligned", () => {
    // "/me 😀 waves": the emoji is one code point (two UTF-16 units) at offset 4.
    const html = renderMeActionHtml({
      body: "/me 😀 waves",
      markup: [{ type: "span", start: 6, end: 11, styles: ["strong"] }],
      actor: "Atlas",
    });
    expect(html).toBe('<p><span class="font-semibold">* Atlas</span> 😀 <strong>waves</strong></p>');
  });

  test("keeps XEP-0372 reference offsets aligned", () => {
    const html = renderMeActionHtml({
      body: "/me shares docs",
      references: [{ type: "data", uri: "https://example.com/docs", begin: 11, end: 15 }],
      actor: "Atlas",
    });
    expect(html).toContain('<a href="https://example.com/docs" target="_blank" rel="noopener noreferrer">docs</a>');
    expect(html).toContain("* Atlas</span> shares ");
  });

  test("an empty action renders just the actor", () => {
    expect(renderMeActionHtml({ body: "/me ", actor: "Atlas" })).toBe('<p><span class="font-semibold">* Atlas</span></p>');
  });
});
