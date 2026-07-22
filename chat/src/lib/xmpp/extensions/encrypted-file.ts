/**
 * XEP-0448: Encryption for Stateless File Sharing.
 *
 * Shared type definitions used by the chat UI and attachment helpers.
 */

/** Closed set of ciphers Waddle understands end-to-end. */
type EncryptedFileCipher =
  | "urn:xmpp:ciphers:aes-128-gcm-nopadding:0"
  | "urn:xmpp:ciphers:aes-256-gcm-nopadding:0";

/**
 * One XEP-0300 hash entry — the ciphertext digest inside `<encrypted/>`
 * or the XEP-0448-mandated plaintext digest inside `<file/>`.
 */
export interface EncryptedFileHash {
  algo: string;
  valueB64: string;
}

export interface WaddleEncryptedFile {
  cipher: EncryptedFileCipher;
  keyB64: string;
  ivB64: string;
  hashes?: EncryptedFileHash[];
  sources?: string[];
}
