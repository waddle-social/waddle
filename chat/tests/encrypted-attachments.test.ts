import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  decryptEncryptedAttachment,
  prepareEncryptedAttachmentUpload,
} from "../src/lib/xmpp/encrypted-attachments";

const originalFetch = globalThis.fetch;

afterEach(() => {
  globalThis.fetch = originalFetch;
});

describe("encrypted attachment runtime", () => {
  test("encrypts uploads and decrypts them back into the original bytes", async () => {
    const original = new File(["hello encrypted world"], "note.txt", { type: "text/plain" });
    const prepared = await prepareEncryptedAttachmentUpload(original);
    const encryptedUrl = "https://files.example.com/note.txt.enc";
    const ciphertext = await prepared.uploadFile.arrayBuffer();

    globalThis.fetch = mock(async (input: RequestInfo | URL) => {
      expect(input.toString()).toBe(encryptedUrl);
      return new Response(ciphertext, {
        status: 200,
        headers: { "Content-Type": "application/octet-stream" },
      });
    }) as typeof fetch;

    const decrypted = await decryptEncryptedAttachment({
      url: encryptedUrl,
      mediaType: prepared.originalMediaType,
      encrypted: {
        ...prepared.encrypted,
        sources: [encryptedUrl],
      },
    });

    expect(await decrypted.text()).toBe("hello encrypted world");
    expect(decrypted.type).toStartWith("text/plain");
  });

  test("advertises the plaintext sha-256 for the XEP-0448 file metadata", async () => {
    const original = new File(["hello encrypted world"], "note.txt", { type: "text/plain" });
    const prepared = await prepareEncryptedAttachmentUpload(original);

    const digest = await crypto.subtle.digest("SHA-256", await original.arrayBuffer());
    const expectedB64 = btoa(String.fromCharCode(...new Uint8Array(digest)));

    // XEP-0448 §2.1: at least one plaintext <hash/> is REQUIRED in the
    // <file/> metadata; it must differ from the ciphertext hash inside
    // <encrypted/> (they digest different bytes).
    expect(prepared.plaintextHashes).toEqual([{ algo: "sha-256", valueB64: expectedB64 }]);
    expect(prepared.encrypted.hashes?.[0]?.valueB64).not.toBe(expectedB64);
  });

  test("rejects encrypted payloads that fail the advertised hash check", async () => {
    const original = new File(["hello encrypted world"], "note.txt", { type: "text/plain" });
    const prepared = await prepareEncryptedAttachmentUpload(original);
    const encryptedUrl = "https://files.example.com/note.txt.enc";
    const ciphertext = await prepared.uploadFile.arrayBuffer();

    globalThis.fetch = mock(async () => new Response(ciphertext, { status: 200 })) as typeof fetch;

    await expect(
      decryptEncryptedAttachment({
        url: encryptedUrl,
        mediaType: prepared.originalMediaType,
        encrypted: {
          ...prepared.encrypted,
          hashes: [{ algo: "sha-256", valueB64: "ZmFrZQ==" }],
          sources: [encryptedUrl],
        },
      }),
    ).rejects.toThrow("integrity check failed");
  });
});
