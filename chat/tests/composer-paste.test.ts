import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { planComposerPaste } from "../src/lib/composer-paste/plan-composer-paste";
import { resolveComposerPaste } from "../src/lib/composer-paste/resolve-composer-paste";
import { fetchPastedGif } from "../src/lib/composer-paste/fetch-pasted-gif";
import {
  parseHasTextWithDom,
  parseImageSourcesWithDom,
  scanHtmlHasText,
  scanHtmlImageSources,
} from "../src/lib/composer-paste/html-image-sources";
import { pastedAnimatedGifUrl } from "../src/lib/composer-paste/pasted-gif-url";
import { MAX_FILE_UPLOAD_BYTES } from "../src/lib/xmpp/file-upload";

const GIF_BYTES = new Uint8Array([0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 1, 0, 1, 0]);
const GIPHY_URL = "https://media2.giphy.com/media/abc123/giphy.gif";

interface FakeClipboard {
  files?: File[];
  items?: Array<{ kind: string; type: string; file: File | null }>;
  data?: Record<string, string>;
}

function clipboard({ files = [], items, data = {} }: FakeClipboard): DataTransfer {
  const itemList = (items ?? files.map((file) => ({ kind: "file", type: file.type, file }))).map((item) => ({
    kind: item.kind,
    type: item.type,
    getAsFile: () => item.file,
  }));
  return {
    files,
    items: itemList,
    getData: (format: string) => data[format] ?? "",
  } as unknown as DataTransfer;
}

function file(name: string, type: string, size = 4): File {
  return new File([new Uint8Array(size)], name, { type });
}

function gifResponse(body: BodyInit | null, headers: Record<string, string> = {}, status = 200): Response {
  return new Response(body, { status, headers: { "content-type": "image/gif", ...headers } });
}

describe("planComposerPaste", () => {
  test("no clipboard data is an editor paste", () => {
    expect(planComposerPaste(null)).toEqual({ kind: "none" });
  });

  test("plain text and URLs stay editor pastes", () => {
    expect(planComposerPaste(clipboard({ data: { "text/plain": "hello" } }))).toEqual({ kind: "none" });
    expect(planComposerPaste(clipboard({ data: { "text/plain": GIPHY_URL } }))).toEqual({ kind: "none" });
  });

  test("rich text without images or files stays an editor paste", () => {
    const plan = planComposerPaste(clipboard({
      data: { "text/plain": "bold", "text/html": "<p><b>bold</b> <a href=\"https://x.test/a.gif\">link</a></p>" },
    }));
    expect(plan).toEqual({ kind: "none" });
  });

  test("a single png becomes a file attachment", () => {
    const png = file("image.png", "image/png");
    expect(planComposerPaste(clipboard({ files: [png] }))).toEqual({ kind: "files", files: [png] });
  });

  test("non-image files such as PDFs are attached", () => {
    const pdf = file("report.pdf", "application/pdf");
    const plan = planComposerPaste(clipboard({ files: [pdf], data: { "text/plain": "report.pdf" } }));
    expect(plan).toEqual({ kind: "files", files: [pdf] });
  });

  test("a file exposed through both files and items is attached once", () => {
    const png = file("image.png", "image/png");
    const sameFileCopy = file("image.png", "image/png");
    const plan = planComposerPaste(clipboard({
      files: [png],
      items: [{ kind: "file", type: "image/png", file: sameFileCopy }],
    }));
    expect(plan).toEqual({ kind: "files", files: [png] });
  });

  test("items are the fallback when files is empty, keeping same-named distinct files", () => {
    const first = file("image.png", "image/png");
    const second = file("image.png", "image/png");
    const plan = planComposerPaste(clipboard({
      items: [
        { kind: "string", type: "text/html", file: null },
        { kind: "file", type: "image/png", file: first },
        { kind: "file", type: "image/png", file: second },
      ],
    }));
    expect(plan).toEqual({ kind: "files", files: [first, second] });
  });

  test("empty files are ignored", () => {
    expect(planComposerPaste(clipboard({ files: [file("folder", "", 0)] }))).toEqual({ kind: "none" });
  });

  test("copied animated GIF plans a fetch with the static frame as fallback", () => {
    const png = file("image.png", "image/png");
    const plan = planComposerPaste(clipboard({
      files: [png],
      data: { "text/html": `<meta charset="utf-8"><img src="${GIPHY_URL}" alt="dance">` },
    }));
    expect(plan).toEqual({ kind: "animated-gif", url: GIPHY_URL, fallback: png });
  });

  test("html-only GIF image plans a fetch without fallback", () => {
    const plan = planComposerPaste(clipboard({
      data: { "text/html": "<img src='https://example.com/cat.GIF?x=1&amp;y=2'>" },
    }));
    expect(plan).toEqual({ kind: "animated-gif", url: "https://example.com/cat.GIF?x=1&y=2", fallback: null });
  });

  test("plain text equal to the image URL still plans the GIF fetch", () => {
    const url = "https://media.tenor.com/abcAAAAC/cat.gif";
    const plan = planComposerPaste(clipboard({
      data: { "text/html": `<img src="${url}">`, "text/plain": url },
    }));
    expect(plan).toEqual({ kind: "animated-gif", url, fallback: null });
  });

  test("a real image/gif file on the clipboard is attached directly", () => {
    const gif = file("dance.gif", "image/gif");
    const plan = planComposerPaste(clipboard({
      files: [gif],
      data: { "text/html": `<img src="${GIPHY_URL}">` },
    }));
    expect(plan).toEqual({ kind: "files", files: [gif] });
  });

  test("non-https GIF images do not plan a fetch", () => {
    const png = file("image.png", "image/png");
    const plan = planComposerPaste(clipboard({
      files: [png],
      data: { "text/html": "<img src=\"http://example.com/cat.gif\">" },
    }));
    expect(plan).toEqual({ kind: "files", files: [png] });
    expect(planComposerPaste(clipboard({ data: { "text/html": "<img src=\"http://example.com/cat.gif\">" } })))
      .toEqual({ kind: "none" });
  });

  test("more than one image in the html is not treated as a GIF copy", () => {
    const plan = planComposerPaste(clipboard({
      data: { "text/html": `<img src="${GIPHY_URL}"><img src="https://example.com/b.gif">` },
    }));
    expect(plan).toEqual({ kind: "none" });
  });

  test("a GIF inside a copied text selection stays an editor paste", () => {
    const plan = planComposerPaste(clipboard({
      data: { "text/html": `<p>look at this <img src="${GIPHY_URL}"></p>`, "text/plain": "look at this" },
    }));
    expect(plan).toEqual({ kind: "none" });
  });

  test("a copied image whose plain text is its alt text still attaches the image", () => {
    const png = file("image.png", "image/png");
    const plan = planComposerPaste(clipboard({
      files: [png],
      data: { "text/html": "<meta charset='utf-8'><img src=\"https://h.test/cat.png\" alt=\"a cat\">", "text/plain": "a cat" },
    }));
    expect(plan).toEqual({ kind: "files", files: [png] });
  });

  test("an image copied with its caption attaches the image and keeps the text", () => {
    const png = file("image.png", "image/png");
    const plan = planComposerPaste(clipboard({
      files: [png],
      data: { "text/html": "<figure><img src=\"https://h.test/cat.png\"><figcaption>my cat</figcaption></figure>", "text/plain": "my cat" },
    }));
    expect(plan).toEqual({ kind: "files-with-text", files: [png] });
  });

  test("office-style text with a rendered preview image pastes as text", () => {
    const plan = planComposerPaste(clipboard({
      files: [file("image.png", "image/png")],
      data: { "text/html": "<table><tr><td>1</td><td>2</td></tr></table>", "text/plain": "1\t2" },
    }));
    expect(plan).toEqual({ kind: "none" });
  });
});

describe("scanHtmlHasText", () => {
  test("image-only markup has no text", () => {
    expect(scanHtmlHasText("<meta charset='utf-8'><img src=\"a.png\" alt=\"a cat\">")).toBe(false);
    expect(scanHtmlHasText("<p>&nbsp;<img src=a.png></p><!-- note --> \n")).toBe(false);
  });

  test("script and style contents are not text", () => {
    expect(scanHtmlHasText("<style>td{color:red}</style><script>x()</script><img src=a.png>")).toBe(false);
  });

  test("the DOMParser path drops raw-text elements before reading body text", () => {
    const removed: string[] = [];
    const parserFor = (text: string) => ({
      parseFromString: () => ({
        body: {
          querySelectorAll: (selector: string) => [{ remove: () => removed.push(selector) }],
          get textContent() {
            return text;
          },
        },
      }),
    }) as unknown as DOMParser;

    expect(parseHasTextWithDom("<img>", parserFor(" \n "))).toBe(false);
    expect(parseHasTextWithDom("<p>hi</p>", parserFor("hi"))).toBe(true);
    expect(removed[0]).toContain("script");
    expect(removed[0]).toContain("style");
  });

  test("character data outside images is text", () => {
    expect(scanHtmlHasText("<table><tr><td>1</td></tr></table>")).toBe(true);
    expect(scanHtmlHasText("<p>look <img src=a.png></p>")).toBe(true);
    expect(scanHtmlHasText("a &lt; b")).toBe(true);
  });
});

describe("scanHtmlImageSources", () => {
  test("reads quoted, single-quoted and unquoted src attributes", () => {
    expect(scanHtmlImageSources("<img src=\"a.gif\"><IMG SRC='b.gif'><img alt=x src=c.gif/>"))
      .toEqual(["a.gif", "b.gif", "c.gif/"]);
  });

  test("does not confuse other attributes that contain src", () => {
    expect(scanHtmlImageSources("<img data-src=\"wrong.gif\" alt='src=\"nope.gif\"' src = \"right.gif\">"))
      .toEqual(["right.gif"]);
  });

  test("ignores '>' inside quoted attribute values", () => {
    expect(scanHtmlImageSources("<img alt=\"a > b\" src=\"x.gif\">")).toEqual(["x.gif"]);
  });

  test("decodes character references and keeps unknown ones", () => {
    expect(scanHtmlImageSources("<img src=\"https://h.test/a.gif?a=1&amp;b=2&#38;c=&#x33;&bogus;&\">"))
      .toEqual(["https://h.test/a.gif?a=1&b=2&c=3&bogus;&"]);
  });

  test("skips comments, raw-text elements and non-img tags", () => {
    const html = "<!-- <img src=\"c.gif\"> --><script>'<img src=\"s.gif\">'</script>"
      + "<imgx src=\"no.gif\"><picture><img src=\"yes.gif\"></picture>";
    expect(scanHtmlImageSources(html)).toEqual(["yes.gif"]);
  });

  test("first duplicate attribute wins and unterminated tags are ignored", () => {
    expect(scanHtmlImageSources("<img src=\"one.gif\" src=\"two.gif\"><img src=\"cut.gif\"")).toEqual(["one.gif"]);
  });

  test("the DOMParser path reads img src attributes", () => {
    const imgs = [{ getAttribute: () => " a.gif " }, { getAttribute: () => null }];
    const parser = {
      parseFromString: () => ({ querySelectorAll: () => imgs }),
    } as unknown as DOMParser;
    expect(parseImageSourcesWithDom("<img>", parser)).toEqual(["a.gif"]);
  });
});

describe("pastedAnimatedGifUrl", () => {
  test("accepts https .gif paths and GIF media hosts", () => {
    expect(pastedAnimatedGifUrl("https://example.com/a/b.gif")).toBe("https://example.com/a/b.gif");
    expect(pastedAnimatedGifUrl("https://i.giphy.com/abc.gif")).toBe("https://i.giphy.com/abc.gif");
    expect(pastedAnimatedGifUrl("https://media.tenor.com/xyz/cat.webp")).toBe("https://media.tenor.com/xyz/cat.webp");
  });

  test("rewrites Giphy webp renditions to their gif sibling", () => {
    expect(pastedAnimatedGifUrl("https://media4.giphy.com/media/id/giphy.webp?cid=1"))
      .toBe("https://media4.giphy.com/media/id/giphy.gif?cid=1");
  });

  test("rejects non-https, credentials, non-GIF paths and look-alike hosts", () => {
    expect(pastedAnimatedGifUrl("http://example.com/a.gif")).toBeNull();
    expect(pastedAnimatedGifUrl("data:image/gif;base64,R0lGOD")).toBeNull();
    expect(pastedAnimatedGifUrl("https://u:p@example.com/a.gif")).toBeNull();
    expect(pastedAnimatedGifUrl("https://example.com/a.png")).toBeNull();
    expect(pastedAnimatedGifUrl("https://giphy.com.evil.test/a.png")).toBeNull();
    expect(pastedAnimatedGifUrl("not a url")).toBeNull();
  });
});

describe("fetchPastedGif", () => {
  test("returns a sanitized .gif File and fetches anonymously", async () => {
    let seen: RequestInit | undefined;
    const fetchImpl = (async (_url: string, init?: RequestInit) => {
      seen = init;
      return gifResponse(GIF_BYTES, { "content-length": String(GIF_BYTES.byteLength) });
    }) as unknown as typeof fetch;
    const gif = await fetchPastedGif("https://example.com/path/My%20Cat!.GIF?x=1", { fetchImpl });
    expect(gif?.name).toBe("My-Cat.gif");
    expect(gif?.type).toBe("image/gif");
    expect(new Uint8Array(await gif!.arrayBuffer())).toEqual(GIF_BYTES);
    expect(seen).toMatchObject({ mode: "cors", credentials: "omit", referrerPolicy: "no-referrer" });
  });

  test("rejects a non-GIF content type", async () => {
    const fetchImpl = (async () => new Response(GIF_BYTES, { headers: { "content-type": "image/webp" } })) as unknown as typeof fetch;
    expect(await fetchPastedGif(GIPHY_URL, { fetchImpl })).toBeNull();
  });

  test("rejects bodies that are not actually GIFs", async () => {
    const fetchImpl = (async () => gifResponse("<html>")) as unknown as typeof fetch;
    expect(await fetchPastedGif(GIPHY_URL, { fetchImpl })).toBeNull();
  });

  test("rejects HTTP errors", async () => {
    const fetchImpl = (async () => gifResponse(GIF_BYTES, {}, 404)) as unknown as typeof fetch;
    expect(await fetchPastedGif(GIPHY_URL, { fetchImpl })).toBeNull();
  });

  test("rejects a declared Content-Length above the upload limit", async () => {
    const fetchImpl = (async () => gifResponse(GIF_BYTES, {
      "content-length": String(MAX_FILE_UPLOAD_BYTES + 1),
    })) as unknown as typeof fetch;
    expect(await fetchPastedGif(GIPHY_URL, { fetchImpl })).toBeNull();
  });

  test("rejects a streamed body larger than the limit without a Content-Length", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(GIF_BYTES);
        controller.enqueue(new Uint8Array(8));
        controller.close();
      },
    });
    const fetchImpl = (async () => gifResponse(stream)) as unknown as typeof fetch;
    expect(await fetchPastedGif(GIPHY_URL, { fetchImpl, maxBytes: GIF_BYTES.byteLength + 4 })).toBeNull();
  });

  test("defaults the size limit to MAX_FILE_UPLOAD_BYTES", async () => {
    const oversized = new Uint8Array(MAX_FILE_UPLOAD_BYTES + 1);
    oversized.set(GIF_BYTES);
    const fetchImpl = (async () => gifResponse(oversized)) as unknown as typeof fetch;
    expect(await fetchPastedGif(GIPHY_URL, { fetchImpl })).toBeNull();
  });

  test("network errors resolve to null", async () => {
    const fetchImpl = (async () => {
      throw new TypeError("Failed to fetch");
    }) as unknown as typeof fetch;
    expect(await fetchPastedGif(GIPHY_URL, { fetchImpl })).toBeNull();
  });

  test("non-https URLs are never fetched", async () => {
    let calls = 0;
    const fetchImpl = (async () => {
      calls += 1;
      return gifResponse(GIF_BYTES);
    }) as unknown as typeof fetch;
    expect(await fetchPastedGif("http://example.com/a.gif", { fetchImpl })).toBeNull();
    expect(await fetchPastedGif("javascript:alert(1)", { fetchImpl })).toBeNull();
    expect(calls).toBe(0);
  });

  test("times out slow hosts", async () => {
    const fetchImpl = ((_url: string, init?: RequestInit) => new Promise<Response>((_resolve, reject) => {
      init?.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
    })) as unknown as typeof fetch;
    expect(await fetchPastedGif(GIPHY_URL, { fetchImpl, timeoutMs: 5 })).toBeNull();
  });

  test("honours the caller's abort signal", async () => {
    const controller = new AbortController();
    controller.abort();
    let aborted = false;
    const fetchImpl = (async (_url: string, init?: RequestInit) => {
      aborted = init?.signal?.aborted ?? false;
      throw new DOMException("aborted", "AbortError");
    }) as unknown as typeof fetch;
    expect(await fetchPastedGif(GIPHY_URL, { fetchImpl, signal: controller.signal })).toBeNull();
    expect(aborted).toBe(true);
  });
});

describe("resolveComposerPaste", () => {
  const png = file("image.png", "image/png");
  const gif = file("dance.gif", "image/gif");

  test("file plans resolve to their files without fetching", async () => {
    const result = await resolveComposerPaste({ kind: "files", files: [png] }, {
      fetchGif: async () => {
        throw new Error("must not fetch");
      },
    });
    expect(result).toEqual({ kind: "files", files: [png] });
  });

  test("a fetched GIF replaces the static fallback", async () => {
    let requested = "";
    const result = await resolveComposerPaste({ kind: "animated-gif", url: GIPHY_URL, fallback: png }, {
      fetchGif: async (url) => {
        requested = url;
        return gif;
      },
    });
    expect(requested).toBe(GIPHY_URL);
    expect(result).toEqual({ kind: "files", files: [gif] });
  });

  test("falls back to the static image when the fetch fails", async () => {
    const result = await resolveComposerPaste(
      { kind: "animated-gif", url: GIPHY_URL, fallback: png },
      { fetchGif: async () => null },
    );
    expect(result).toEqual({ kind: "files", files: [png] });
  });

  test("falls back to the URL as text when there is no static image", async () => {
    const result = await resolveComposerPaste(
      { kind: "animated-gif", url: GIPHY_URL, fallback: null },
      { fetchGif: async () => null },
    );
    expect(result).toEqual({ kind: "text", text: GIPHY_URL });
  });

  test("an aborted paste attaches nothing", async () => {
    const controller = new AbortController();
    const result = await resolveComposerPaste(
      { kind: "animated-gif", url: GIPHY_URL, fallback: png },
      {
        signal: controller.signal,
        fetchGif: async () => {
          controller.abort();
          return gif;
        },
      },
    );
    expect(result).toEqual({ kind: "files", files: [] });
  });
});

describe("ChatEditor paste wiring", () => {
  const source = readFileSync(new URL("../src/components/chat/ChatEditor.vue", import.meta.url), "utf8");

  test("handlePaste returns the synchronous pasteHandler decision", () => {
    expect(source).toContain("pasteHandler?: (event: ClipboardEvent) => boolean;");
    expect(source).toContain("handlePaste: (_view, event) => props.pasteHandler?.(event) ?? false,");
  });

  test("the fire-and-forget paste emit is gone", () => {
    expect(source).not.toContain("emit(\"paste\"");
    expect(source).not.toMatch(/paste: \[event: ClipboardEvent\]/);
  });
});
