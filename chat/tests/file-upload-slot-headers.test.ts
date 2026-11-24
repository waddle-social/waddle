import { afterEach, expect, test } from "bun:test";
import { uploadFile } from "../src/lib/xmpp/file-upload";

const originalXhr = globalThis.XMLHttpRequest;

afterEach(() => {
  globalThis.XMLHttpRequest = originalXhr;
});

/** Upload `file` against a fake XHR and a slot carrying `putHeaders`; return the headers sent. */
async function headersSentFor(putHeaders: Array<{ name: string; value: string }>) {
  const sent: Array<[string, string]> = [];
  class FakeXhr {
    status = 201;
    upload = {};
    onload: (() => void) | null = null;
    onerror: (() => void) | null = null;
    open() {}
    setRequestHeader(name: string, value: string) {
      sent.push([name, value]);
    }
    send() {
      this.onload?.();
    }
  }
  globalThis.XMLHttpRequest = FakeXhr as unknown as typeof XMLHttpRequest;

  const xmpp = {
    request_upload_slot: async () => ({
      put_url: "https://example.test/put",
      get_url: "https://example.test/get",
      put_headers: putHeaders,
    }),
  };
  const file = new File(["hello"], "a.png", { type: "image/png" });
  await uploadFile(xmpp as never, file, "upload.example.test");
  return sent;
}

test("PUT sends the file's Content-Type and only XEP-0363 allowed slot headers", async () => {
  const sent = await headersSentFor([
    { name: "Content-Type", value: "text/evil" },
    { name: "Authorization", value: "Bearer x" },
    { name: "X-Other", value: "y" },
  ]);
  expect(sent).toEqual([
    ["Content-Type", "image/png"],
    ["Authorization", "Bearer x"],
  ]);
});

test("PUT strips CR/LF from slot header names and values and keeps repeated headers in order", async () => {
  const sent = await headersSentFor([
    { name: "Cookie", value: "a=1" },
    { name: "Auth\r\norization", value: "Bearer x\r\ny" },
    { name: "Cookie", value: "b=\n2" },
  ]);
  expect(sent).toEqual([
    ["Content-Type", "image/png"],
    ["Cookie", "a=1"],
    ["Authorization", "Bearer xy"],
    ["Cookie", "b=2"],
  ]);
});
