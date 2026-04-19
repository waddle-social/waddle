import { describe, expect, test } from "bun:test";
import { Registry, XMLElement } from "stanza/jxt";
import encryptedFileDefinitions, {
  NS_ESFS_0,
  NS_HASHES_2,
  type WaddleEncryptedFile,
} from "../src/lib/xmpp/extensions/encrypted-file";

const NS_SFS_0 = "urn:xmpp:sfs:0";
const NS_URL_DATA = "http://jabber.org/protocol/url-data";

function newRegistry(): Registry {
  const r = new Registry();
  r.define(encryptedFileDefinitions);
  return r;
}

function sample(): WaddleEncryptedFile {
  return {
    cipher: "urn:xmpp:ciphers:aes-256-gcm-nopadding:0",
    keyB64: "a2V5",
    ivB64: "aXY=",
    hashes: [{ algo: "sha-256", valueB64: "aGFzaA==" }],
    sources: ["https://files.example.com/blob.enc"],
  };
}

describe("encrypted file (XEP-0448) jxt extension", () => {
  test("export emits cipher attr, key/iv/hash children, and nested sources", () => {
    const r = newRegistry();
    const enc = sample();
    const xml = r.export("message.encryptedFiles", enc as unknown as Parameters<Registry["export"]>[1]);
    expect(xml).toBeDefined();
    expect(xml!.name).toBe("encrypted");
    expect(xml!.getNamespace()).toBe(NS_ESFS_0);
    expect(xml!.getAttribute("cipher")).toBe(enc.cipher);
    expect(xml!.getChild("key", NS_ESFS_0)?.getText()).toBe(enc.keyB64);
    expect(xml!.getChild("iv", NS_ESFS_0)?.getText()).toBe(enc.ivB64);

    const hash = xml!.getChild("hash", NS_HASHES_2);
    expect(hash).toBeDefined();
    expect(hash!.getAttribute("algo")).toBe("sha-256");
    expect(hash!.getText()).toBe("aGFzaA==");

    const sources = xml!.getChild("sources", NS_SFS_0);
    expect(sources).toBeDefined();
    const urlData = sources!.getChild("url-data", NS_URL_DATA);
    expect(urlData).toBeDefined();
    expect(urlData!.getAttribute("target")).toBe("https://files.example.com/blob.enc");
  });

  test("import round-trips the typed value", () => {
    const r = newRegistry();
    const enc = sample();
    const xml = r.export("message.encryptedFiles", enc as unknown as Parameters<Registry["export"]>[1])!;
    const imported = r.import(xml) as WaddleEncryptedFile;
    expect(imported.cipher).toBe(enc.cipher);
    expect(imported.keyB64).toBe(enc.keyB64);
    expect(imported.ivB64).toBe(enc.ivB64);
    expect(imported.hashes).toEqual(enc.hashes);
    expect(imported.sources).toEqual(enc.sources);
  });

  test("import handles AES-128 cipher", () => {
    const r = newRegistry();
    const xml = new XMLElement("encrypted", {
      xmlns: NS_ESFS_0,
      cipher: "urn:xmpp:ciphers:aes-128-gcm-nopadding:0",
    });
    const key = new XMLElement("key", { xmlns: NS_ESFS_0 });
    key.appendChild("k");
    xml.appendChild(key);
    const iv = new XMLElement("iv", { xmlns: NS_ESFS_0 });
    iv.appendChild("v");
    xml.appendChild(iv);
    const sources = new XMLElement("sources", { xmlns: NS_SFS_0 });
    const ud = new XMLElement("url-data", {
      xmlns: NS_URL_DATA,
      target: "https://example.com/x",
    });
    sources.appendChild(ud);
    xml.appendChild(sources);

    const imported = r.import(xml) as WaddleEncryptedFile;
    expect(imported.cipher).toBe("urn:xmpp:ciphers:aes-128-gcm-nopadding:0");
    expect(imported.keyB64).toBe("k");
    expect(imported.ivB64).toBe("v");
    expect(imported.sources).toEqual(["https://example.com/x"]);
    expect(imported.hashes).toBeUndefined();
  });

  test("multiple encrypted files round-trip on a message envelope", () => {
    const r = newRegistry();
    const enc1 = sample();
    const enc2: WaddleEncryptedFile = {
      ...sample(),
      sources: ["https://files.example.com/two.enc"],
    };

    const xml1 = r.export("message.encryptedFiles", enc1 as unknown as Parameters<Registry["export"]>[1])!;
    const xml2 = r.export("message.encryptedFiles", enc2 as unknown as Parameters<Registry["export"]>[1])!;

    const i1 = r.import(xml1) as WaddleEncryptedFile;
    const i2 = r.import(xml2) as WaddleEncryptedFile;
    expect(i1.sources).toEqual(enc1.sources!);
    expect(i2.sources).toEqual(enc2.sources!);
  });
});
