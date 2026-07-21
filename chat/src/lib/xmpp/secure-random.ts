const UUID_BYTE_LENGTH = 16;

function formatUuidV4(bytes: Uint8Array): string {
  if (bytes.length !== UUID_BYTE_LENGTH) {
    throw new TypeError("A UUID requires exactly 16 random bytes");
  }
  bytes[6] = (bytes[6]! & 0x0f) | 0x40;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0"));
  return `${hex.slice(0, 4).join("")}-${hex.slice(4, 6).join("")}-${hex.slice(6, 8).join("")}-${hex.slice(8, 10).join("")}-${hex.slice(10).join("")}`;
}

/**
 * Generates an unguessable browser identity for XMPP resources and durable
 * ownership claims. These values fence session-resume authority, so a weak
 * pseudo-random fallback would let one context impersonate another.
 */
export function secureRandomUuid(): string {
  const webCrypto = globalThis.crypto;
  if (typeof webCrypto?.randomUUID === "function") return webCrypto.randomUUID();
  if (typeof webCrypto?.getRandomValues !== "function") {
    throw new DOMException("Secure Web Crypto is unavailable", "NotSupportedError");
  }
  return formatUuidV4(webCrypto.getRandomValues(new Uint8Array(UUID_BYTE_LENGTH)));
}

/** Test-only deterministic formatter for UUID version and variant coverage. */
export function __formatUuidV4ForTesting(bytes: Uint8Array): string {
  return formatUuidV4(new Uint8Array(bytes));
}
