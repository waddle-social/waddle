import { describe, expect, test } from "bun:test";
import XMLElement from "stanza/jxt/Element";
import {
  NS_WADDLE_PREVIEW_0,
  importPreview,
} from "../src/lib/xmpp/extensions/preview";

const NS_REFERENCE_0 = "urn:xmpp:reference:0";

function makeReferenceWithPreview(previewXml: XMLElement): XMLElement {
  const ref = new XMLElement("reference", {
    xmlns: NS_REFERENCE_0,
    type: "data",
    uri: "https://example.com/a",
    begin: "0",
    end: "22",
  });
  ref.appendChild(previewXml);
  return ref;
}

function makePreviewXml(
  overrides: Partial<{
    xmlns: string;
    url: string;
    title: string;
    description: string;
    siteName: string;
    type: string;
    image: { src: string; width?: string; height?: string } | null;
  }> = {},
): XMLElement {
  const {
    xmlns = NS_WADDLE_PREVIEW_0,
    url = "https://example.com/a",
    title = "Title",
    description = "Desc",
    siteName = "Site",
    type = "article",
    image = { src: "https://cdn.example.com/a.png", width: "1200", height: "630" },
  } = overrides;

  const preview = new XMLElement("preview", { xmlns, url });

  if (title !== "") {
    const t = new XMLElement("title", { xmlns });
    t.appendChild(title);
    preview.appendChild(t);
  }
  if (description !== "") {
    const d = new XMLElement("description", { xmlns });
    d.appendChild(description);
    preview.appendChild(d);
  }
  if (siteName !== "") {
    const s = new XMLElement("site-name", { xmlns });
    s.appendChild(siteName);
    preview.appendChild(s);
  }
  if (type !== "") {
    const ty = new XMLElement("type", { xmlns });
    ty.appendChild(type);
    preview.appendChild(ty);
  }
  if (image) {
    const attrs: Record<string, string> = { xmlns, src: image.src };
    if (image.width) attrs.width = image.width;
    if (image.height) attrs.height = image.height;
    preview.appendChild(new XMLElement("image", attrs));
  }
  return preview;
}

describe("importPreview", () => {
  test("reads title/description/site-name/type/url and image", () => {
    const ref = makeReferenceWithPreview(makePreviewXml());
    const preview = importPreview(ref);
    expect(preview).toEqual({
      url: "https://example.com/a",
      title: "Title",
      description: "Desc",
      siteName: "Site",
      type: "article",
      image: { src: "https://cdn.example.com/a.png", width: "1200", height: "630" },
    });
  });

  test("returns undefined when no preview child present", () => {
    const ref = new XMLElement("reference", {
      xmlns: NS_REFERENCE_0,
      type: "data",
      uri: "https://example.com/a",
    });
    expect(importPreview(ref)).toBeUndefined();
  });

  test("silently drops preview in wrong namespace", () => {
    const ref = makeReferenceWithPreview(
      makePreviewXml({ xmlns: "urn:bogus:link-preview:99" }),
    );
    expect(importPreview(ref)).toBeUndefined();
  });

  test("silently drops preview with missing url attribute", () => {
    const preview = new XMLElement("preview", { xmlns: NS_WADDLE_PREVIEW_0 });
    preview.appendChild(
      (() => {
        const t = new XMLElement("title", { xmlns: NS_WADDLE_PREVIEW_0 });
        t.appendChild("T");
        return t;
      })(),
    );
    const ref = new XMLElement("reference", {
      xmlns: NS_REFERENCE_0,
      type: "data",
      uri: "https://example.com/a",
    });
    ref.appendChild(preview);
    expect(importPreview(ref)).toBeUndefined();
  });

  test("silently drops image with non-http scheme", () => {
    const ref = makeReferenceWithPreview(
      makePreviewXml({ image: { src: "javascript:alert(1)" } }),
    );
    const preview = importPreview(ref);
    expect(preview?.image).toBeUndefined();
  });

  test("truncates long title to 200 chars", () => {
    const long = "a".repeat(500);
    const ref = makeReferenceWithPreview(makePreviewXml({ title: long }));
    expect(importPreview(ref)?.title?.length).toBe(200);
  });

  test("truncates long description to 300 chars", () => {
    const long = "b".repeat(1000);
    const ref = makeReferenceWithPreview(makePreviewXml({ description: long }));
    expect(importPreview(ref)?.description?.length).toBe(300);
  });

  test("truncates long site-name to 100 chars", () => {
    const long = "c".repeat(500);
    const ref = makeReferenceWithPreview(makePreviewXml({ siteName: long }));
    expect(importPreview(ref)?.siteName?.length).toBe(100);
  });

  test("works with only required fields", () => {
    const preview = new XMLElement("preview", {
      xmlns: NS_WADDLE_PREVIEW_0,
      url: "https://example.com/a",
    });
    const title = new XMLElement("title", { xmlns: NS_WADDLE_PREVIEW_0 });
    title.appendChild("T");
    preview.appendChild(title);
    const ref = new XMLElement("reference", {
      xmlns: NS_REFERENCE_0,
      type: "data",
      uri: "https://example.com/a",
    });
    ref.appendChild(preview);
    expect(importPreview(ref)).toEqual({ url: "https://example.com/a", title: "T" });
  });

  test("silently drops preview with unparseable url", () => {
    const ref = makeReferenceWithPreview(makePreviewXml({ url: "not a url" }));
    expect(importPreview(ref)).toBeUndefined();
  });
});
