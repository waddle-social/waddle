import { markSensitiveUrlForTelemetry } from "@/lib/telemetry";
import type { EncryptedFileHash, WaddleEncryptedFile } from "./extensions/encrypted-file";

const AES_GCM_NAME = "AES-GCM";
const AES_GCM_IV_BYTES = 12;
const AES_128_GCM_CIPHER = "urn:xmpp:ciphers:aes-128-gcm-nopadding:0";
const AES_256_GCM_CIPHER = "urn:xmpp:ciphers:aes-256-gcm-nopadding:0";
const SHA_256 = "sha-256";

interface PreparedEncryptedAttachmentUpload {
  uploadFile: File;
  encrypted: WaddleEncryptedFile;
  originalName: string;
  originalMediaType: string;
  originalSize: number;
  /**
   * XEP-0448-mandated plaintext content hashes for the `<file/>`
   * metadata (distinct from the ciphertext hash inside `<encrypted/>`).
   */
  plaintextHashes: EncryptedFileHash[];
}

interface EncryptedAttachmentDescriptor {
  url: string;
  name?: string;
  mediaType?: string;
  encrypted?: WaddleEncryptedFile;
}

function requireSubtleCrypto(): SubtleCrypto {
  if (!globalThis.crypto?.subtle) {
    throw new Error("Encrypted attachments require Web Crypto support.");
  }
  return globalThis.crypto.subtle;
}

function fileName(file: File | Blob): string {
  return file instanceof File ? file.name : `image-${Date.now()}.png`;
}

function fileMediaType(file: File | Blob): string {
  return file.type || "application/octet-stream";
}

function encryptedUploadName(name: string): string {
  return name.endsWith(".enc") ? name : `${name}.enc`;
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunkSize));
  }
  return btoa(binary);
}

function base64ToBytes(value: string): Uint8Array {
  const binary = atob(value);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    out[i] = binary.charCodeAt(i);
  }
  return out;
}

function toArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

async function sha256Base64(data: BufferSource): Promise<string> {
  const subtle = requireSubtleCrypto();
  const digest = await subtle.digest("SHA-256", data);
  return bytesToBase64(new Uint8Array(digest));
}

function sourceUrls(descriptor: EncryptedAttachmentDescriptor): string[] {
  const encryptedSources = descriptor.encrypted?.sources?.filter((value) => value.length > 0) ?? [];
  return encryptedSources.length > 0 ? encryptedSources : [descriptor.url];
}

function encryptionCacheKey(descriptor: EncryptedAttachmentDescriptor): string {
  const encrypted = descriptor.encrypted;
  if (!encrypted) return descriptor.url;
  return [
    descriptor.url,
    encrypted.cipher,
    encrypted.keyB64,
    encrypted.ivB64,
    ...(encrypted.hashes?.map((hash) => `${hash.algo}:${hash.valueB64}`) ?? []),
    ...(encrypted.sources ?? []),
  ].join("|");
}

function validateCipherKeyLength(cipher: string, keyBytes: Uint8Array): void {
  if (cipher === AES_128_GCM_CIPHER && keyBytes.byteLength !== 16) {
    throw new Error("Encrypted attachment metadata does not contain a valid AES-128 key.");
  }
  if (cipher === AES_256_GCM_CIPHER && keyBytes.byteLength !== 32) {
    throw new Error("Encrypted attachment metadata does not contain a valid AES-256 key.");
  }
}

export function hasEncryptedAttachmentMetadata(
  attachment: EncryptedAttachmentDescriptor,
): attachment is EncryptedAttachmentDescriptor & { encrypted: WaddleEncryptedFile } {
  return !!attachment.encrypted?.cipher && !!attachment.encrypted?.keyB64 && !!attachment.encrypted?.ivB64;
}

export function encryptedAttachmentKey(attachment: EncryptedAttachmentDescriptor): string {
  return encryptionCacheKey(attachment);
}

export async function prepareEncryptedAttachmentUpload(
  file: File | Blob,
): Promise<PreparedEncryptedAttachmentUpload> {
  const subtle = requireSubtleCrypto();
  const originalName = fileName(file);
  const originalMediaType = fileMediaType(file);
  const originalSize = file.size;
  const clearBytes = await file.arrayBuffer();
  const iv = globalThis.crypto.getRandomValues(new Uint8Array(AES_GCM_IV_BYTES));
  const rawKey = globalThis.crypto.getRandomValues(new Uint8Array(32));
  const cryptoKey = await subtle.importKey("raw", toArrayBuffer(rawKey), { name: AES_GCM_NAME }, false, ["encrypt"]);
  const encryptedBytes = await subtle.encrypt({ name: AES_GCM_NAME, iv: toArrayBuffer(iv) }, cryptoKey, clearBytes);
  const encryptedBlob = new File(
    [encryptedBytes],
    encryptedUploadName(originalName),
    { type: "application/octet-stream" },
  );
  const hashB64 = await sha256Base64(encryptedBytes);
  const plaintextHashB64 = await sha256Base64(clearBytes);

  return {
    uploadFile: encryptedBlob,
    encrypted: {
      cipher: AES_256_GCM_CIPHER,
      keyB64: bytesToBase64(rawKey),
      ivB64: bytesToBase64(iv),
      hashes: [{ algo: SHA_256, valueB64: hashB64 }],
    },
    originalName,
    originalMediaType,
    originalSize,
    plaintextHashes: [{ algo: SHA_256, valueB64: plaintextHashB64 }],
  };
}

async function decryptCiphertext(
  ciphertext: ArrayBuffer,
  encrypted: WaddleEncryptedFile,
): Promise<ArrayBuffer> {
  const subtle = requireSubtleCrypto();
  const keyBytes = base64ToBytes(encrypted.keyB64);
  const ivBytes = base64ToBytes(encrypted.ivB64);
  validateCipherKeyLength(encrypted.cipher, keyBytes);

  const sha256Hashes = encrypted.hashes?.filter((hash) => hash.algo.toLowerCase() === SHA_256) ?? [];
  if (sha256Hashes.length > 0) {
    const actualHash = await sha256Base64(ciphertext);
    if (!sha256Hashes.some((hash) => hash.valueB64 === actualHash)) {
      throw new Error("Encrypted attachment integrity check failed.");
    }
  }

  const cryptoKey = await subtle.importKey("raw", toArrayBuffer(keyBytes), { name: AES_GCM_NAME }, false, ["decrypt"]);
  return subtle.decrypt({ name: AES_GCM_NAME, iv: toArrayBuffer(ivBytes) }, cryptoKey, ciphertext);
}

const inFlightDecryptionCache = new Map<string, Promise<Blob>>();

export async function decryptEncryptedAttachment(
  attachment: EncryptedAttachmentDescriptor & { encrypted: WaddleEncryptedFile },
): Promise<Blob> {
  const cacheKey = encryptionCacheKey(attachment);
  const existing = inFlightDecryptionCache.get(cacheKey);
  if (existing) return existing;

  const task = (async () => {
    let lastError: unknown;
    for (const url of sourceUrls(attachment)) {
      try {
        markSensitiveUrlForTelemetry(url);
        const response = await fetch(url);
        if (!response.ok) {
          throw new Error(`Failed to fetch encrypted attachment with HTTP ${response.status}`);
        }
        const ciphertext = await response.arrayBuffer();
        const plaintext = await decryptCiphertext(ciphertext, attachment.encrypted);
        return new Blob([plaintext], { type: attachment.mediaType || "application/octet-stream" });
      } catch (error) {
        lastError = error;
      }
    }
    throw lastError ?? new Error("Unable to fetch encrypted attachment.");
  })();

  inFlightDecryptionCache.set(cacheKey, task);
  try {
    return await task;
  } finally {
    if (inFlightDecryptionCache.get(cacheKey) === task) {
      inFlightDecryptionCache.delete(cacheKey);
    }
  }
}
