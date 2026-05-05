/**
 * XEP-0448: Encryption for Stateless File Sharing.
 *
 * Shared type definitions used by the chat UI and attachment helpers.
 */
const NS_ESFS_0 = "urn:xmpp:esfs:0";
const NS_HASHES_2 = "urn:xmpp:hashes:2";

/** Closed set of ciphers Waddle understands end-to-end. */
export type EncryptedFileCipher =
  | "urn:xmpp:ciphers:aes-128-gcm-nopadding:0"
  | "urn:xmpp:ciphers:aes-256-gcm-nopadding:0";

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
