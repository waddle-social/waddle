/**
 * XEP-0448: Encryption for Stateless File Sharing.
 *
 * Mirrors the Rust types in `server/crates/waddle-xmpp/src/xep/xep0448.rs`.
 *
 * Wire format:
 *   <encrypted xmlns='urn:xmpp:esfs:0' cipher='urn:xmpp:ciphers:aes-256-gcm-nopadding:0'>
 *     <key>BASE64</key>
 *     <iv>BASE64</iv>
 *     <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>BASE64</hash>
 *     <sources xmlns='urn:xmpp:sfs:0'>
 *       <url-data xmlns='http://jabber.org/protocol/url-data' target='https://...'/>
 *     </sources>
 *   </encrypted>
 */
import type { DefinitionOptions, FieldDefinition } from "stanza/jxt";
import { attribute } from "stanza/jxt";
import XMLElement from "stanza/jxt/Element";

export const NS_ESFS_0 = "urn:xmpp:esfs:0";
export const NS_HASHES_2 = "urn:xmpp:hashes:2";
const NS_SFS_0 = "urn:xmpp:sfs:0";
const NS_URL_DATA = "http://jabber.org/protocol/url-data";

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

const keyField: FieldDefinition<string> = {
  importer(xml: XMLElement) {
    return xml.getChild("key", NS_ESFS_0)?.getText();
  },
  exporter(xml: XMLElement, value: string) {
    if (!value) return;
    const el = new XMLElement("key", { xmlns: NS_ESFS_0 });
    el.appendChild(value);
    xml.appendChild(el);
  },
};

const ivField: FieldDefinition<string> = {
  importer(xml: XMLElement) {
    return xml.getChild("iv", NS_ESFS_0)?.getText();
  },
  exporter(xml: XMLElement, value: string) {
    if (!value) return;
    const el = new XMLElement("iv", { xmlns: NS_ESFS_0 });
    el.appendChild(value);
    xml.appendChild(el);
  },
};

const hashesField: FieldDefinition<EncryptedFileHash[]> = {
  importer(xml: XMLElement) {
    const out: EncryptedFileHash[] = [];
    for (const child of xml.getChildren("hash", NS_HASHES_2)) {
      const algo = child.getAttribute("algo");
      if (!algo) continue;
      out.push({ algo, valueB64: child.getText() });
    }
    return out.length > 0 ? out : undefined;
  },
  exporter(xml: XMLElement, value: EncryptedFileHash[]) {
    if (!Array.isArray(value)) return;
    for (const h of value) {
      const el = new XMLElement("hash", { xmlns: NS_HASHES_2, algo: h.algo });
      el.appendChild(h.valueB64);
      xml.appendChild(el);
    }
  },
};

const sourcesField: FieldDefinition<string[]> = {
  importer(xml: XMLElement) {
    const sources = xml.getChild("sources", NS_SFS_0);
    if (!sources) return undefined;
    const urls: string[] = [];
    for (const ud of sources.getChildren("url-data", NS_URL_DATA)) {
      const target = ud.getAttribute("target");
      if (target) urls.push(target);
    }
    return urls.length > 0 ? urls : undefined;
  },
  exporter(xml: XMLElement, value: string[]) {
    if (!Array.isArray(value) || value.length === 0) return;
    const sources = new XMLElement("sources", { xmlns: NS_SFS_0 });
    for (const url of value) {
      const ud = new XMLElement("url-data", { xmlns: NS_URL_DATA, target: url });
      sources.appendChild(ud);
    }
    xml.appendChild(sources);
  },
};

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.encryptedFiles", multiple: true }],
    element: "encrypted",
    fields: {
      cipher: attribute("cipher"),
      keyB64: keyField,
      ivB64: ivField,
      hashes: hashesField,
      sources: sourcesField,
    },
    namespace: NS_ESFS_0,
  },
];

export default definitions;
